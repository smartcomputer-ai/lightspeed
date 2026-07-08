//! Core command admission for external session requests.

use crate::{
    CommandError, CommandRejection, CommandRejectionKind, ContextEntrySource, ContextEvent,
    CoreAgentCommand, CoreAgentEvent, CoreAgentEventProposal, CoreAgentJoins,
    CoreAgentLifecycleEvent, CoreAgentState, CoreAgentStatus, DomainError, MessageStatus,
    PromiseEvent, PromiseResolution, RunEvent, RunRequestSource, RunSource, RunStatus,
    ToolConfigEvent,
    core::components::{
        config::{validate_config_update_for_state, validate_run_config_for_state},
        tooling::{
            validate_default_tool_target_clear, validate_default_tool_target_set, validate_tool_map,
        },
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
            config
                .validate_provider_compatibility()
                .map_err(command_rejection_from_domain)?;
            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config }),
            )])
        }
        CoreAgentCommand::PatchSessionConfig {
            expected_revision,
            patch,
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
            let config = patch.apply_to(current);
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
                ) {
                    Some(SubmissionMatch::Identical) => return Ok(Vec::new()),
                    Some(SubmissionMatch::Different) => {
                        return reject(
                            CommandRejectionKind::DuplicateSubmission,
                            format!(
                                "submission id {submission_id} was already used by a run \
                                     with different input or run config"
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
            let source = match request.source {
                RunRequestSource::Input { input } => {
                    if input.is_empty() {
                        return reject(
                            CommandRejectionKind::InvariantViolation,
                            "run input must contain at least one entry",
                        );
                    }
                    crate::core::components::context::validate_run_input_entries(&input)
                        .map_err(command_rejection_from_domain)?;
                    RunSource::Input { input }
                }
                RunRequestSource::Context { keys } => {
                    let triggers =
                        crate::core::components::context::validate_run_trigger_context_keys(
                            state, &keys,
                        )
                        .map_err(command_rejection_from_domain)?;
                    RunSource::Context { triggers }
                }
            };
            if source.input().is_empty() && source.context_triggers().is_empty() {
                return reject(
                    CommandRejectionKind::InvariantViolation,
                    "run source must contain input entries or trigger context keys",
                );
            }
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
                    origin: crate::RunOrigin::Requested,
                    source,
                    run_config: request.run_config,
                    config_revision: state.lifecycle.config_revision,
                    notify_on_terminal: request.notify_on_terminal,
                })),
            )])
        }
        CoreAgentCommand::SubmitMessage(message) => {
            if let Some(submission_id) = message.submission_id.as_ref() {
                use crate::core::components::run::{
                    SubmissionMatch, match_existing_message_submission,
                };
                match match_existing_message_submission(state, submission_id, &message.input) {
                    Some(SubmissionMatch::Identical) => return Ok(Vec::new()),
                    Some(SubmissionMatch::Different) => {
                        return reject(
                            CommandRejectionKind::DuplicateSubmission,
                            format!(
                                "submission id {submission_id} was already used by a different \
                                     command or message input"
                            ),
                        );
                    }
                    None => {}
                }
            }
            require_open(state)?;
            require_no_pending_compaction(
                state,
                "message cannot be delivered while context compaction is pending",
            )?;
            if message.input.is_empty() {
                return reject(
                    CommandRejectionKind::InvariantViolation,
                    "message input must contain at least one entry",
                );
            }
            crate::core::components::context::validate_run_input_entries(&message.input)
                .map_err(command_rejection_from_domain)?;
            let current_config = state.lifecycle.config.as_ref().ok_or_else(|| {
                CommandError::Domain(DomainError::InvariantViolation(
                    "open session is missing config".to_owned(),
                ))
            })?;
            let run_config = current_config.run.clone();
            validate_run_config_for_state(state, &run_config)
                .map_err(command_rejection_from_domain)?;
            if state.runs.active.as_ref().is_some_and(|run| {
                run.status == RunStatus::Parked
                    && run
                        .parked_await
                        .as_ref()
                        .is_some_and(|parked| parked.spec.mailbox)
            }) {
                let next_message_id =
                    state
                        .id_cursors
                        .last_message_id
                        .checked_add(1)
                        .ok_or_else(|| {
                            CommandError::Domain(DomainError::InvariantViolation(
                                "message id cursor exhausted".to_owned(),
                            ))
                        })?;
                let joins = CoreAgentJoins {
                    submission_id: message.submission_id.clone(),
                    ..CoreAgentJoins::default()
                };
                let input = message.input;
                return Ok(vec![CoreAgentEventProposal::new(
                    joins,
                    CoreAgentEvent::Run(RunEvent::MessageBuffered {
                        message_id: crate::MessageId::new(next_message_id),
                        submission_id: message.submission_id,
                        submission_digest: crate::message_submission_digest(&input),
                        input,
                        run_config,
                        config_revision: state.lifecycle.config_revision,
                    }),
                )]);
            }
            let next_run_id = state.id_cursors.last_run_id.checked_add(1).ok_or_else(|| {
                CommandError::Domain(DomainError::InvariantViolation(
                    "run id cursor exhausted".to_owned(),
                ))
            })?;
            let next_run_id = crate::RunId::new(next_run_id);
            let joins = CoreAgentJoins {
                submission_id: message.submission_id.clone(),
                run_id: Some(next_run_id),
                ..CoreAgentJoins::default()
            };
            Ok(vec![CoreAgentEventProposal::new(
                joins,
                CoreAgentEvent::Run(RunEvent::Accepted(crate::AcceptedRunEvent {
                    run_id: next_run_id,
                    submission_id: message.submission_id,
                    origin: crate::RunOrigin::Message,
                    source: RunSource::Input {
                        input: message.input,
                    },
                    run_config,
                    config_revision: state.lifecycle.config_revision,
                    notify_on_terminal: Vec::new(),
                })),
            )])
        }
        CoreAgentCommand::UpsertContext { key, entry } => {
            require_open(state)?;
            require_no_pending_compaction(
                state,
                "context cannot be edited while context compaction is pending",
            )?;
            crate::core::components::context::validate_external_context_edit(&key, &entry)
                .map_err(command_rejection_from_domain)?;
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
        CoreAgentCommand::RemoveContext { key } => {
            require_open(state)?;
            require_no_pending_compaction(
                state,
                "context cannot be edited while context compaction is pending",
            )?;
            crate::core::components::context::validate_external_context_key(&key)
                .map_err(command_rejection_from_domain)?;
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
                    || state
                        .runs
                        .messages
                        .iter()
                        .any(|message| message.status == MessageStatus::Buffered)
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
                for message in state
                    .runs
                    .messages
                    .iter()
                    .filter(|message| message.status == MessageStatus::Buffered)
                {
                    proposals.push(CoreAgentEventProposal::new(
                        CoreAgentJoins {
                            submission_id: message.submission_id.clone(),
                            ..CoreAgentJoins::default()
                        },
                        CoreAgentEvent::Run(RunEvent::MessageCancelled {
                            message_id: message.message_id,
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
        CoreAgentCommand::RequestRunSteering { input } => {
            require_open(state)?;
            let active_run = active_run_for_command(state)?;
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
            if let Some(active_run) = state.runs.active.as_ref() {
                if active_run.run_id == run_id {
                    if matches!(
                        active_run.status,
                        RunStatus::Cancelling | RunStatus::CancellingGrace
                    ) {
                        return Ok(Vec::new());
                    }
                    if matches!(active_run.status, RunStatus::Active | RunStatus::Parked) {
                        return Ok(vec![CoreAgentEventProposal::new(
                            CoreAgentJoins {
                                run_id: Some(active_run.run_id),
                                ..CoreAgentJoins::default()
                            },
                            CoreAgentEvent::Run(RunEvent::CancellationRequested {
                                run_id: active_run.run_id,
                            }),
                        )]);
                    }
                    return Ok(Vec::new());
                }
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
        CoreAgentCommand::ResumeAwait(command) => {
            require_open(state)?;
            crate::core::drive::resume_await_proposals(state, command, observed_at_ms)
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
        CoreAgentCommand::SetDefaultToolTarget { target } => {
            require_open(state)?;
            validate_default_tool_target_set(&target).map_err(command_rejection_from_domain)?;

            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::ToolConfig(ToolConfigEvent::DefaultTargetSet { target }),
            )])
        }
        CoreAgentCommand::ClearDefaultToolTarget { namespace } => {
            require_open(state)?;
            validate_default_tool_target_clear(&namespace)
                .map_err(command_rejection_from_domain)?;

            Ok(vec![CoreAgentEventProposal::new(
                CoreAgentJoins::default(),
                CoreAgentEvent::ToolConfig(ToolConfigEvent::DefaultTargetCleared { namespace }),
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

fn active_run_for_command(state: &CoreAgentState) -> Result<&crate::ActiveRun, CommandError> {
    let Some(active_run) = state.runs.active.as_ref() else {
        return reject(
            CommandRejectionKind::MissingActiveRun,
            "command requires an active run",
        );
    };
    if active_run.status != RunStatus::Active {
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
