//! Core command admission for external session requests.

use crate::{
    ApprovalContinuation, ApprovalEvent, ApprovalStatus, CommandError, CommandRejection,
    CommandRejectionKind, ContextEntrySource, ContextEvent, CoreAgentCommand, CoreAgentEvent,
    CoreAgentEventProposal, CoreAgentJoins, CoreAgentLifecycleEvent, CoreAgentState,
    CoreAgentStatus, DomainError, PromiseEvent, PromiseResolution, RunEvent, RunRequestSource,
    RunSource, RunStatus, ToolConfigEvent, WorkflowToolConfigEvent,
    core::components::{
        config::{validate_config_update_for_state, validate_run_config_for_state},
        tooling::validate_tool_map,
    },
};

pub fn admit_command(
    state: &CoreAgentState,
    command: CoreAgentCommand,
    observed_at_ms: u64,
) -> Result<Vec<CoreAgentEventProposal>, CommandError> {
    match command {
        CoreAgentCommand::OpenSession { config } => {
            if state.lifecycle.status != CoreAgentStatus::New {
                return reject(
                    CommandRejectionKind::CoreAgentState,
                    "session can only be opened from new state",
                );
            }
            config.validate().map_err(command_rejection_from_domain)?;
            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config }),
            )])
        }
        CoreAgentCommand::OpenManagedSession {
            config,
            session_universe_id,
            workflow_tools,
        } => {
            if state.lifecycle.status != CoreAgentStatus::New {
                return reject(
                    CommandRejectionKind::CoreAgentState,
                    "session can only be opened from new state",
                );
            }
            config.validate().map_err(command_rejection_from_domain)?;
            let admitted = workflow_tools
                .admit(session_universe_id)
                .map_err(command_rejection_from_domain)?;
            Ok(vec![
                CoreAgentEventProposal::new(
                    CoreAgentJoins::default(),
                    CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config }),
                ),
                CoreAgentEventProposal::new(
                    CoreAgentJoins::default(),
                    CoreAgentEvent::WorkflowToolConfig(
                        WorkflowToolConfigEvent::ManagedBindingsAdmitted {
                            session_universe_id: admitted.session_universe_id,
                            declaration_version: admitted.version,
                            lifecycle_controller: admitted.lifecycle_controller,
                            creation_fingerprint: admitted.creation_fingerprint,
                            bindings: admitted.bindings,
                        },
                    ),
                ),
            ])
        }
        CoreAgentCommand::AdmitSystemWorkflowTool {
            session_universe_id,
            declaration,
        } => {
            require_open(state)?;
            let binding = crate::WorkflowToolBinding::admit(
                session_universe_id,
                declaration.definition,
                declaration.target,
                declaration.completion,
            )
            .map_err(command_rejection_from_domain)?;
            let tool_id = &binding.definition.tool_id;
            if let Some(existing) = state.workflow_tools.bindings.get(tool_id) {
                if existing == &binding && state.workflow_tools.system_binding_ids.contains(tool_id)
                {
                    return Ok(Vec::new());
                }
                return reject(
                    CommandRejectionKind::InvalidConfiguration,
                    format!(
                        "system workflow tool {tool_id} conflicts with an existing immutable binding"
                    ),
                );
            }
            if let Some(existing) = state
                .workflow_tools
                .bindings
                .values()
                .find(|existing| existing.definition.tool.name == binding.definition.tool.name)
            {
                return reject(
                    CommandRejectionKind::InvalidConfiguration,
                    format!(
                        "system workflow tool {tool_id} name {} collides with workflow tool {}",
                        binding.definition.tool.name, existing.definition.tool_id
                    ),
                );
            }
            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::WorkflowToolConfig(
                    WorkflowToolConfigEvent::SystemBindingAdmitted {
                        binding: Box::new(binding),
                    },
                ),
            )])
        }
        CoreAgentCommand::ReplaceSessionConfig {
            expected_revision,
            config,
        } => {
            require_open(state)?;
            require_no_active_or_queued_work(
                state,
                "session config can only change while no run or compaction is active or queued",
            )?;
            if let Some(expected_revision) = expected_revision {
                let actual_revision = state.lifecycle.config_revision;
                if expected_revision != actual_revision {
                    return reject(
                        CommandRejectionKind::InvalidConfiguration,
                        format!(
                            "expected config revision {}, got {}",
                            expected_revision, actual_revision
                        ),
                    );
                }
            }
            let current = state.lifecycle.config.as_ref().ok_or_else(|| {
                CommandError::Domain(DomainError::InvariantViolation(
                    "open session is missing config".to_owned(),
                ))
            })?;
            if &config == current {
                // Replacing with the identical document is an idempotent
                // no-op; the revision stays untouched.
                return Ok(Vec::new());
            }
            validate_config_update_for_state(state, &config)
                .map_err(command_rejection_from_domain)?;
            let revision = state
                .lifecycle
                .config_revision
                .checked_add(1)
                .ok_or_else(|| {
                    CommandError::Domain(DomainError::InvariantViolation(
                        "config revision exhausted".to_owned(),
                    ))
                })?;
            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::ConfigChanged {
                    config,
                    revision,
                }),
            )])
        }
        CoreAgentCommand::RequestRun(request) => {
            // Duplicate detection precedes every other check so a retried
            // submission resolves idempotently even when session state has
            // moved on (e.g. the original run completed or the session is
            // compacting).
            if let Some(submission_id) = request.submission_id.as_ref() {
                use crate::core::components::run::{
                    SubmissionMatch, match_existing_run_submission,
                };
                match match_existing_run_submission(
                    state,
                    submission_id,
                    &request.source,
                    &request.run_config,
                    &request.notify_on_terminal,
                ) {
                    Some(SubmissionMatch::Identical) => return Ok(Vec::new()),
                    Some(SubmissionMatch::Different) => {
                        return reject(
                            CommandRejectionKind::DuplicateSubmission,
                            format!(
                                "submission id {submission_id} was already used by a run \
                                     with different input, run config, or terminal notification"
                            ),
                        );
                    }
                    None => {}
                }
            }
            require_open(state)?;
            require_no_pending_compaction(
                state,
                "run cannot be requested while context compaction is pending",
            )?;
            validate_run_config_for_state(state, &request.run_config)
                .map_err(command_rejection_from_domain)?;
            let next_run_id = state.id_cursors.last_run_id.checked_add(1).ok_or_else(|| {
                CommandError::Domain(DomainError::InvariantViolation(
                    "run id cursor exhausted".to_owned(),
                ))
            })?;
            let next_run_id = crate::RunId::new(next_run_id);
            let RunRequestSource::Input { input } = request.source;
            if input.is_empty() {
                return reject(
                    CommandRejectionKind::InvariantViolation,
                    "run input must contain at least one entry",
                );
            }
            crate::core::components::context::validate_run_input_entries(&input)
                .map_err(command_rejection_from_domain)?;
            let source = RunSource::Input { input };
            let joins = CoreAgentJoins {
                submission_id: request.submission_id.clone(),
                run_id: Some(next_run_id),
                ..CoreAgentJoins::default()
            };
            Ok(vec![CoreAgentEventProposal::new(
                joins,
                CoreAgentEvent::Run(RunEvent::Accepted(crate::AcceptedRunEvent {
                    run_id: next_run_id,
                    submission_id: request.submission_id,
                    source,
                    run_config: request.run_config,
                    config_revision: state.lifecycle.config_revision,
                    notify_on_terminal: request.notify_on_terminal,
                })),
            )])
        }
        CoreAgentCommand::UpsertContext {
            expected_revision,
            key,
            entry,
        } => {
            require_open(state)?;
            require_no_pending_compaction(
                state,
                "context cannot be edited while context compaction is pending",
            )?;
            crate::core::components::context::validate_external_context_edit(&key, &entry)
                .map_err(command_rejection_from_domain)?;
            validate_expected_context_revision(state, expected_revision)?;
            if crate::core::components::context::context_upsert_is_noop(state, &key, &entry) {
                return Ok(Vec::new());
            }
            let entries = crate::core::components::context::context_entries_from_inputs(
                state,
                vec![(Some(key), ContextEntrySource::ContextEdit, entry)],
            )
            .map_err(CommandError::Domain)?;
            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::Context(ContextEvent::EntriesApplied {
                    base_revision: state.context.revision,
                    entries,
                }),
            )])
        }
        CoreAgentCommand::ReplaceContextPrefix {
            expected_revision,
            key_prefix,
            entries,
        } => {
            require_open(state)?;
            require_no_pending_compaction(
                state,
                "context cannot be edited while context compaction is pending",
            )?;
            crate::core::components::context::validate_external_context_prefix_replacement(
                &key_prefix,
                &entries,
            )
            .map_err(command_rejection_from_domain)?;
            validate_expected_context_revision(state, expected_revision)?;
            if crate::core::components::context::context_prefix_replacement_is_noop(
                state,
                &key_prefix,
                &entries,
            ) {
                return Ok(Vec::new());
            }
            let entries = crate::core::components::context::context_entries_from_inputs(
                state,
                entries
                    .into_iter()
                    .map(|(key, entry)| (Some(key), ContextEntrySource::ContextEdit, entry))
                    .collect(),
            )
            .map_err(CommandError::Domain)?;
            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::Context(ContextEvent::KeyPrefixReplaced {
                    base_revision: state.context.revision,
                    key_prefix,
                    entries,
                }),
            )])
        }
        CoreAgentCommand::RemoveContext {
            expected_revision,
            key,
        } => {
            require_open(state)?;
            require_no_pending_compaction(
                state,
                "context cannot be edited while context compaction is pending",
            )?;
            crate::core::components::context::validate_external_context_key(&key)
                .map_err(command_rejection_from_domain)?;
            validate_expected_context_revision(state, expected_revision)?;
            crate::core::components::context::validate_context_key_exists(state, &key)
                .map_err(unknown_reference_rejection_from_domain)?;
            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::Context(ContextEvent::KeysRemoved {
                    base_revision: state.context.revision,
                    keys: vec![key],
                }),
            )])
        }
        CoreAgentCommand::CompactContext => {
            require_open(state)?;
            crate::core::components::context::manual_compaction_requested_proposal(state)
                .map(|proposal| vec![proposal])
                .map_err(command_rejection_from_domain)
        }
        CoreAgentCommand::CloseSession { force } => {
            if force && state.lifecycle.status == crate::CoreAgentStatus::Closed {
                // Force-close is a recovery surface; retrying against an
                // already-closed session is an idempotent no-op.
                return Ok(Vec::new());
            }
            require_open(state)?;
            if !force
                && (state.runs.active.is_some()
                    || !state.runs.queued.is_empty()
                    || state.context.pending_compaction
                    || state
                        .promises
                        .pending()
                        .any(|promise| matches!(promise.scope, crate::PromiseScope::Session)))
            {
                return reject(
                    CommandRejectionKind::ActiveWork,
                    "session cannot close with active work",
                );
            }
            let mut proposals = Vec::new();
            if force {
                if let Some(active_run) = state.runs.active.as_ref() {
                    proposals.push(CoreAgentEventProposal::new(
                        CoreAgentJoins {
                            run_id: Some(active_run.run_id),
                            ..CoreAgentJoins::default()
                        },
                        CoreAgentEvent::Run(RunEvent::ForceCancelled {
                            run_id: active_run.run_id,
                        }),
                    ));
                }
                for queued in &state.runs.queued {
                    proposals.push(CoreAgentEventProposal::new(
                        CoreAgentJoins {
                            run_id: Some(queued.run_id),
                            ..CoreAgentJoins::default()
                        },
                        CoreAgentEvent::Run(RunEvent::QueuedCancelled {
                            run_id: queued.run_id,
                        }),
                    ));
                }
                for promise in state
                    .promises
                    .pending()
                    .filter(|promise| matches!(promise.scope, crate::PromiseScope::Session))
                {
                    proposals.push(CoreAgentEventProposal::new(
                        CoreAgentJoins::default(),
                        CoreAgentEvent::Promise(PromiseEvent::Cancelled {
                            promise_id: promise.promise_id.clone(),
                        }),
                    ));
                }
            }
            proposals.push(CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Closed),
            ));
            Ok(proposals)
        }
        CoreAgentCommand::RequestRunSteering { run_id, input } => {
            require_open(state)?;
            let active_run = active_run_for_command(state)?;
            if active_run.run_id != run_id {
                return reject(
                    CommandRejectionKind::UnknownReference,
                    format!("steering target run {run_id} is no longer active"),
                );
            }
            crate::core::components::context::validate_steering_input_entries(&input)
                .map_err(command_rejection_from_domain)?;
            let next_steering_id = state
                .id_cursors
                .last_steering_id
                .checked_add(1)
                .ok_or_else(|| {
                    CommandError::Domain(DomainError::InvariantViolation(
                        "steering id cursor exhausted".to_owned(),
                    ))
                })?;
            let joins = CoreAgentJoins {
                run_id: Some(active_run.run_id),
                ..CoreAgentJoins::default()
            };
            Ok(vec![CoreAgentEventProposal::new(
                joins,
                CoreAgentEvent::Run(RunEvent::SteeringAccepted {
                    run_id: active_run.run_id,
                    steering_id: crate::SteeringId::new(next_steering_id),
                    input,
                }),
            )])
        }
        CoreAgentCommand::CancelRun { run_id } => {
            require_open(state)?;
            if let Some(active_run) = state.runs.active.as_ref()
                && active_run.run_id == run_id
            {
                if active_run.status == RunStatus::Cancelling {
                    return Ok(Vec::new());
                }
                if matches!(active_run.status, RunStatus::Active | RunStatus::Parked) {
                    let mut proposals = vec![CoreAgentEventProposal::new(
                        CoreAgentJoins {
                            run_id: Some(active_run.run_id),
                            ..CoreAgentJoins::default()
                        },
                        CoreAgentEvent::Run(RunEvent::CancellationRequested {
                            run_id: active_run.run_id,
                        }),
                    )];
                    proposals.extend(active_run.pending_approvals().map(|record| {
                        CoreAgentEventProposal::new(
                            CoreAgentJoins {
                                run_id: Some(run_id),
                                ..CoreAgentJoins::default()
                            },
                            CoreAgentEvent::Approval(ApprovalEvent::Cancelled {
                                approval_id: record.request.approval_id.clone(),
                                run_id,
                            }),
                        )
                    }));
                    return Ok(proposals);
                }
                return Ok(Vec::new());
            }
            if state
                .runs
                .queued
                .iter()
                .any(|queued| queued.run_id == run_id)
            {
                return Ok(vec![CoreAgentEventProposal::new(
                    CoreAgentJoins {
                        run_id: Some(run_id),
                        ..CoreAgentJoins::default()
                    },
                    CoreAgentEvent::Run(RunEvent::QueuedCancelled { run_id }),
                )]);
            }
            Ok(Vec::new())
        }
        CoreAgentCommand::DecideApproval(command) => {
            require_open(state)?;
            crate::core::components::approval::validate_note(command.note.as_deref())
                .map_err(command_rejection_from_domain)?;
            let active_run = state
                .runs
                .active
                .as_ref()
                .filter(|run| run.run_id == command.run_id)
                .ok_or_else(|| {
                    CommandError::Rejected(CommandRejection::new(
                        CommandRejectionKind::UnknownReference,
                        format!("run {} is not active", command.run_id),
                    ))
                })?;
            let record = active_run
                .approvals
                .get(&command.approval_id)
                .ok_or_else(|| {
                    CommandError::Rejected(CommandRejection::new(
                        CommandRejectionKind::UnknownReference,
                        format!("unknown approval {}", command.approval_id),
                    ))
                })?;
            if record.request.run_id != command.run_id {
                return reject(
                    CommandRejectionKind::UnknownReference,
                    format!(
                        "approval {} does not belong to run {}",
                        command.approval_id, command.run_id
                    ),
                );
            }
            if record.status != ApprovalStatus::Pending {
                return reject(
                    CommandRejectionKind::InvalidConfiguration,
                    format!("approval {} is already terminal", command.approval_id),
                );
            }
            let Some(active) = state.runs.active.as_ref() else {
                return reject(
                    CommandRejectionKind::MissingActiveRun,
                    "approval decision requires an active run",
                );
            };
            if active.run_id != command.run_id
                || !matches!(active.status, RunStatus::Active | RunStatus::Parked)
            {
                return reject(
                    CommandRejectionKind::ActiveWork,
                    "approval decision does not target the accepting active run",
                );
            }
            let provider_expectation = match &record.request.continuation {
                ApprovalContinuation::OpenAiMcp {
                    provider_request_id,
                } => Some((
                    provider_request_id.as_str(),
                    command.decision == crate::ApprovalDecision::Approved,
                )),
                ApprovalContinuation::NativeMcp { .. } => None,
            };
            if let Some((expected_provider_id, expected_approve)) = provider_expectation {
                match command.response.as_ref().map(|response| &response.kind) {
                    Some(crate::ContextEntryKind::McpApprovalResponse {
                        approval_request_id,
                        approve,
                    }) if approval_request_id == expected_provider_id
                        && *approve == expected_approve => {}
                    _ => {
                        return reject(
                            CommandRejectionKind::InvariantViolation,
                            "approval response context does not match the pending continuation",
                        );
                    }
                }
            } else if command.response.is_some() {
                return reject(
                    CommandRejectionKind::InvariantViolation,
                    "native MCP approval decisions must not add provider response context",
                );
            }
            let entries = if let Some(response) = command.response {
                crate::core::components::context::context_entries_from_inputs(
                    state,
                    vec![(
                        None,
                        ContextEntrySource::ApprovalDecision {
                            run_id: command.run_id,
                            approval_id: command.approval_id.clone(),
                        },
                        response,
                    )],
                )?
            } else {
                Vec::new()
            };
            let mut proposals = vec![CoreAgentEventProposal::new(
                CoreAgentJoins {
                    run_id: Some(command.run_id),
                    ..CoreAgentJoins::default()
                },
                CoreAgentEvent::Approval(ApprovalEvent::Decided {
                    approval_id: command.approval_id,
                    run_id: command.run_id,
                    decision: command.decision,
                    note: command.note,
                    decided_by: command.decided_by,
                }),
            )];
            if !entries.is_empty() {
                proposals.push(CoreAgentEventProposal::new(
                    CoreAgentJoins {
                        run_id: Some(command.run_id),
                        ..CoreAgentJoins::default()
                    },
                    CoreAgentEvent::Context(ContextEvent::EntriesApplied {
                        base_revision: state.context.revision,
                        entries,
                    }),
                ));
            }
            Ok(proposals)
        }
        CoreAgentCommand::ResolvePromise {
            promise_id,
            resolution,
        } => {
            let Some(promise) = state.promises.promises.get(&promise_id) else {
                return reject(
                    CommandRejectionKind::UnknownReference,
                    format!("unknown promise {promise_id}"),
                );
            };
            if promise.status.is_terminal() {
                // First writer won; later deliveries are idempotent no-ops.
                return Ok(Vec::new());
            }
            let event = match resolution {
                PromiseResolution::Resolved { payload_ref } => PromiseEvent::Resolved {
                    promise_id,
                    payload_ref,
                },
                PromiseResolution::Failed { error_ref } => PromiseEvent::Failed {
                    promise_id,
                    error_ref,
                },
                PromiseResolution::Cancelled => PromiseEvent::Cancelled { promise_id },
            };
            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::Promise(event),
            )])
        }
        CoreAgentCommand::FailWorkflowToolDelivery {
            invocation_id,
            error_ref,
        } => {
            let Some(invocation) = state.workflow_tools.emissions.get(&invocation_id) else {
                return reject(
                    CommandRejectionKind::UnknownReference,
                    format!("unknown workflow tool invocation {invocation_id}"),
                );
            };
            match state.workflow_tools.delivery_failures.get(&invocation_id) {
                // First terminal failure won; retried admission is a no-op.
                Some(existing) if existing == &error_ref => return Ok(Vec::new()),
                Some(_) => {
                    return reject(
                        CommandRejectionKind::CoreAgentState,
                        format!(
                            "workflow tool invocation {invocation_id} already has a different delivery failure"
                        ),
                    );
                }
                None => {}
            }
            let mut proposals = vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::WorkflowTool(crate::WorkflowToolEvent::DeliveryFailed {
                    invocation_id: invocation_id.clone(),
                    error_ref: error_ref.clone(),
                }),
            )];
            // A dead receiver must never leave an unresolvable pending
            // promise: fail every still-pending keyed completion promise of
            // this invocation in the same append.
            if let Some(promises) = &invocation.completion_promises {
                for promise_id in promises.values() {
                    let Some(promise) = state.promises.promises.get(promise_id) else {
                        continue;
                    };
                    if promise.status.is_terminal() {
                        continue;
                    }
                    proposals.push(CoreAgentEventProposal::new(
                        CoreAgentJoins::default(),
                        CoreAgentEvent::Promise(PromiseEvent::Failed {
                            promise_id: promise_id.clone(),
                            error_ref: Some(error_ref.clone()),
                        }),
                    ));
                }
            }
            Ok(proposals)
        }
        CoreAgentCommand::FailWorkflowToolStart {
            invocation_id,
            error_ref,
        } => {
            let Some(invocation) = state.workflow_tools.start_requests.get(&invocation_id) else {
                return reject(
                    CommandRejectionKind::UnknownReference,
                    format!("unknown workflow tool start intent {invocation_id}"),
                );
            };
            match state.workflow_tools.start_failures.get(&invocation_id) {
                // First terminal failure won; retried admission is a no-op.
                Some(existing) if existing == &error_ref => return Ok(Vec::new()),
                Some(_) => {
                    return reject(
                        CommandRejectionKind::CoreAgentState,
                        format!(
                            "workflow tool start intent {invocation_id} already has a different terminal failure"
                        ),
                    );
                }
                None => {}
            }
            let mut proposals = vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::WorkflowTool(crate::WorkflowToolEvent::StartFailed {
                    invocation_id: invocation_id.clone(),
                    error_ref: error_ref.clone(),
                }),
            )];
            // An unstartable execution must never leave an unresolvable
            // pending promise.
            if let Some(promises) = &invocation.completion_promises {
                for promise_id in promises.values() {
                    let Some(promise) = state.promises.promises.get(promise_id) else {
                        continue;
                    };
                    if promise.status.is_terminal() {
                        continue;
                    }
                    proposals.push(CoreAgentEventProposal::new(
                        CoreAgentJoins::default(),
                        CoreAgentEvent::Promise(PromiseEvent::Failed {
                            promise_id: promise_id.clone(),
                            error_ref: Some(error_ref.clone()),
                        }),
                    ));
                }
            }
            Ok(proposals)
        }
        CoreAgentCommand::ForceCancelRun { run_id } => {
            require_open(state)?;
            let Some(active_run) = state.runs.active.as_ref() else {
                // The run already reached a terminal state (or never
                // existed); the watchdog retry is an idempotent no-op.
                return Ok(Vec::new());
            };
            if active_run.run_id != run_id {
                return Ok(Vec::new());
            }
            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins {
                    run_id: Some(run_id),
                    ..CoreAgentJoins::default()
                },
                CoreAgentEvent::Run(RunEvent::ForceCancelled { run_id }),
            )])
        }
        CoreAgentCommand::ResumeToolBatch(command) => {
            require_open(state)?;
            crate::core::drive::resume_tool_batch_proposals(state, command, observed_at_ms)
                .map_err(command_rejection_from_domain)
        }
        CoreAgentCommand::ReplaceTools {
            expected_revision,
            tools,
        } => {
            require_open(state)?;
            validate_expected_tool_revision(state, expected_revision)?;
            validate_tool_map(&tools).map_err(command_rejection_from_domain)?;

            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::ToolConfig(ToolConfigEvent::ToolsReplaced {
                    base_revision: state.tooling.revision,
                    tools,
                }),
            )])
        }
        CoreAgentCommand::PatchTools {
            expected_revision,
            patch,
        } => {
            require_open(state)?;
            validate_expected_tool_revision(state, expected_revision)?;
            if patch.is_empty() {
                return Ok(Vec::new());
            }
            patch
                .apply_to(&state.tooling.tools)
                .map_err(command_rejection_from_domain)?;

            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::ToolConfig(ToolConfigEvent::ToolsPatched {
                    base_revision: state.tooling.revision,
                    patch,
                }),
            )])
        }
        CoreAgentCommand::SetActiveEnvironment { environment_id } => {
            require_open(state)?;
            if state
                .lifecycle
                .config
                .as_ref()
                .and_then(|config| config.features.environments.as_ref())
                .is_none()
            {
                return reject(
                    CommandRejectionKind::InvalidConfiguration,
                    "active environment requires the environments feature",
                );
            }
            if state.environment.active_environment_id.as_ref() == Some(&environment_id) {
                return Ok(Vec::new());
            }

            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::Environment(crate::EnvironmentEvent::ActiveEnvironmentSet {
                    environment_id,
                }),
            )])
        }
        CoreAgentCommand::ClearActiveEnvironment => {
            require_open(state)?;
            if state.environment.active_environment_id.is_none() {
                return Ok(Vec::new());
            }

            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::Environment(crate::EnvironmentEvent::ActiveEnvironmentCleared),
            )])
        }
    }
}

fn require_no_active_or_queued_work(
    state: &CoreAgentState,
    message: &'static str,
) -> Result<(), CommandError> {
    if state.runs.active.is_some()
        || !state.runs.queued.is_empty()
        || state.context.pending_compaction
    {
        reject(CommandRejectionKind::ActiveWork, message)
    } else {
        Ok(())
    }
}

fn validate_expected_tool_revision(
    state: &CoreAgentState,
    expected_revision: Option<u64>,
) -> Result<(), CommandError> {
    if let Some(expected_revision) = expected_revision {
        let actual_revision = state.tooling.revision;
        if expected_revision != actual_revision {
            return reject(
                CommandRejectionKind::InvalidConfiguration,
                format!(
                    "expected tool revision {}, got {}",
                    expected_revision, actual_revision
                ),
            );
        }
    }
    Ok(())
}

fn validate_expected_context_revision(
    state: &CoreAgentState,
    expected_revision: Option<u64>,
) -> Result<(), CommandError> {
    if let Some(expected) = expected_revision {
        let actual = state.context.revision;
        if expected != actual {
            return Err(CommandError::Rejected(
                CommandRejection::context_revision_conflict(expected, actual),
            ));
        }
    }
    Ok(())
}

fn require_no_pending_compaction(
    state: &CoreAgentState,
    message: &'static str,
) -> Result<(), CommandError> {
    if state.context.pending_compaction {
        reject(CommandRejectionKind::ActiveWork, message)
    } else {
        Ok(())
    }
}

fn require_open(state: &CoreAgentState) -> Result<(), CommandError> {
    if state.lifecycle.status == CoreAgentStatus::Open {
        Ok(())
    } else {
        reject(CommandRejectionKind::CoreAgentState, "session must be open")
    }
}

/// The run a steering command targets: the active run while it is `Active`
/// or `Parked`. A parked run (model-chosen `await`) accepts steering without
/// waking; the steering materializes in the first turn after it resumes
/// A cancelling run no longer accepts commands.
fn active_run_for_command(state: &CoreAgentState) -> Result<&crate::ActiveRun, CommandError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return reject(
            CommandRejectionKind::MissingActiveRun,
            "command requires an active run",
        );
    };
    if !matches!(active_run.status, RunStatus::Active | RunStatus::Parked) {
        return reject(
            CommandRejectionKind::ActiveWork,
            "active run is not accepting commands",
        );
    }
    Ok(active_run)
}

fn reject<T>(kind: CommandRejectionKind, message: impl Into<String>) -> Result<T, CommandError> {
    Err(CommandError::Rejected(CommandRejection::new(kind, message)))
}

fn command_rejection_from_domain(error: DomainError) -> CommandError {
    let kind = match error {
        DomainError::ProviderCompatibility(_) => CommandRejectionKind::ProviderCompatibility,
        DomainError::InvariantViolation(_) | DomainError::EventOrdering(_) => {
            CommandRejectionKind::InvariantViolation
        }
    };
    CommandError::Rejected(CommandRejection::new(kind, error.to_string()))
}

fn unknown_reference_rejection_from_domain(error: DomainError) -> CommandError {
    CommandError::Rejected(CommandRejection::new(
        CommandRejectionKind::UnknownReference,
        error.to_string(),
    ))
}
