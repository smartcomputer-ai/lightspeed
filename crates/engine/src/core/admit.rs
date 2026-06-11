//! Core command admission for external session requests.

use crate::{
    AdmitCommand, CommandError, CommandRejection, CommandRejectionKind, ContextEntrySource,
    ContextEvent, CoreAgentCommand, CoreAgentEventKind, CoreAgentEventProposal, CoreAgentJoins,
    CoreAgentLifecycleEvent, CoreAgentState, CoreAgentStatus, DomainError, RunEvent, RunStatus,
    ToolConfigEvent,
    core::components::{
        config::{validate_config_update_for_state, validate_run_config_for_state},
        tooling::{
            validate_default_tool_target_clear, validate_default_tool_target_set, validate_tool_map,
        },
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreAdmitCommand;

impl AdmitCommand for CoreAdmitCommand {
    fn admit(
        &self,
        state: &CoreAgentState,
        command: CoreAgentCommand,
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
                    CoreAgentEventKind::Lifecycle(CoreAgentLifecycleEvent::Opened { config }),
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
                    CoreAgentEventKind::Lifecycle(CoreAgentLifecycleEvent::ConfigChanged {
                        config,
                        revision,
                    }),
                )])
            }
            CoreAgentCommand::RequestRun {
                submission_id,
                input,
                run_config,
            } => {
                require_open(state)?;
                require_no_pending_compaction(
                    state,
                    "run cannot be requested while context compaction is pending",
                )?;
                validate_run_config_for_state(state, &run_config)
                    .map_err(command_rejection_from_domain)?;
                crate::core::components::context::validate_run_input_entries(&input)
                    .map_err(command_rejection_from_domain)?;
                let next_run_id = state.id_cursors.last_run_id.checked_add(1).ok_or_else(|| {
                    CommandError::Domain(DomainError::InvariantViolation(
                        "run id cursor exhausted".to_owned(),
                    ))
                })?;
                let joins = CoreAgentJoins {
                    submission_id: submission_id.clone(),
                    run_id: Some(crate::RunId::new(next_run_id)),
                    ..CoreAgentJoins::default()
                };
                Ok(vec![CoreAgentEventProposal::new(
                    joins,
                    CoreAgentEventKind::Run(RunEvent::Accepted {
                        run_id: crate::RunId::new(next_run_id),
                        submission_id,
                        input,
                        run_config,
                        config_revision: state.lifecycle.config_revision,
                    }),
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
                let entries = crate::core::components::context::context_entries_from_inputs(
                    state,
                    vec![(Some(key), ContextEntrySource::ContextEdit, entry)],
                )
                .map_err(CommandError::Domain)?;
                Ok(vec![CoreAgentEventProposal::new(
                    CoreAgentJoins::default(),
                    CoreAgentEventKind::Context(ContextEvent::EntriesApplied {
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
                    CoreAgentEventKind::Context(ContextEvent::KeyPrefixReplaced {
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
                crate::core::components::context::validate_context_key_exists(state, &key)
                    .map_err(unknown_reference_rejection_from_domain)?;
                Ok(vec![CoreAgentEventProposal::new(
                    CoreAgentJoins::default(),
                    CoreAgentEventKind::Context(ContextEvent::KeysRemoved {
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
            CoreAgentCommand::CloseSession => {
                require_open(state)?;
                if state.runs.active.is_some()
                    || !state.runs.queued.is_empty()
                    || state.context.pending_compaction
                {
                    return reject(
                        CommandRejectionKind::ActiveWork,
                        "session cannot close with active work",
                    );
                }
                Ok(vec![CoreAgentEventProposal::new(
                    CoreAgentJoins::default(),
                    CoreAgentEventKind::Lifecycle(CoreAgentLifecycleEvent::Closed),
                )])
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
                    CoreAgentEventKind::Run(RunEvent::SteeringAccepted {
                        run_id: active_run.run_id,
                        steering_id: crate::SteeringId::new(next_steering_id),
                        input,
                    }),
                )])
            }
            CoreAgentCommand::RequestRunCancellation => {
                require_open(state)?;
                let active_run = active_run_for_command(state)?;
                let joins = CoreAgentJoins {
                    run_id: Some(active_run.run_id),
                    ..CoreAgentJoins::default()
                };
                Ok(vec![CoreAgentEventProposal::new(
                    joins,
                    CoreAgentEventKind::Run(RunEvent::CancellationRequested {
                        run_id: active_run.run_id,
                    }),
                )])
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
                    CoreAgentEventKind::ToolConfig(ToolConfigEvent::ToolsReplaced {
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
                    CoreAgentEventKind::ToolConfig(ToolConfigEvent::ToolsPatched {
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
                    CoreAgentEventKind::ToolConfig(ToolConfigEvent::DefaultTargetSet { target }),
                )])
            }
            CoreAgentCommand::ClearDefaultToolTarget { namespace } => {
                require_open(state)?;
                validate_default_tool_target_clear(&namespace)
                    .map_err(command_rejection_from_domain)?;

                Ok(vec![CoreAgentEventProposal::new(
                    CoreAgentJoins::default(),
                    CoreAgentEventKind::ToolConfig(ToolConfigEvent::DefaultTargetCleared {
                        namespace,
                    }),
                )])
            }
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
