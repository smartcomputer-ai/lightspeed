//! Core command admission for external session requests.

use crate::{
    AdmitCommand, CommandError, CommandRejection, CommandRejectionKind, CoreAgentCommand,
    CoreAgentEventKind, CoreAgentEventProposal, CoreAgentJoins, CoreAgentLifecycleEvent,
    CoreAgentState, CoreAgentStatus, DomainError, RunEvent, RunStatus, ToolConfigEvent,
    core::components::{
        config::{validate_config_update_for_state, validate_run_config_for_state},
        skills::validate_activations,
        tooling::{
            validate_default_tool_target_clear, validate_default_tool_target_set,
            validate_profile_exists, validate_registry_keeps_active_profile,
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
                    "session config can only change while no run is active or queued",
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
                input_ref,
                run_config,
            } => {
                require_open(state)?;
                validate_run_config_for_state(state, &run_config)
                    .map_err(command_rejection_from_domain)?;
                let joins = CoreAgentJoins {
                    submission_id: submission_id.clone(),
                    ..CoreAgentJoins::default()
                };
                Ok(vec![CoreAgentEventProposal::new(
                    joins,
                    CoreAgentEventKind::Run(RunEvent::Queued {
                        submission_id,
                        input_ref,
                        run_config,
                    }),
                )])
            }
            CoreAgentCommand::CloseSession => {
                require_open(state)?;
                if state.runs.active.is_some() || !state.runs.queued.is_empty() {
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
            CoreAgentCommand::RequestRunSteering { input_ref } => {
                require_open(state)?;
                let active_run = active_run_for_command(state)?;
                let joins = CoreAgentJoins {
                    run_id: Some(active_run.run_id),
                    ..CoreAgentJoins::default()
                };
                Ok(vec![CoreAgentEventProposal::new(
                    joins,
                    CoreAgentEventKind::Run(RunEvent::SteeringAdded {
                        run_id: active_run.run_id,
                        input_ref,
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
            CoreAgentCommand::SetToolRegistry { registry } => {
                require_open(state)?;
                registry.validate().map_err(command_rejection_from_domain)?;
                validate_registry_keeps_active_profile(state, &registry)
                    .map_err(command_rejection_from_domain)?;

                Ok(vec![CoreAgentEventProposal::new(
                    CoreAgentJoins::default(),
                    CoreAgentEventKind::ToolConfig(ToolConfigEvent::RegistryChanged { registry }),
                )])
            }
            CoreAgentCommand::SelectToolProfile { profile_id } => {
                require_open(state)?;
                validate_profile_exists(&state.tooling.registry, &profile_id)
                    .map_err(unknown_reference_rejection_from_domain)?;

                Ok(vec![CoreAgentEventProposal::new(
                    CoreAgentJoins::default(),
                    CoreAgentEventKind::ToolConfig(ToolConfigEvent::ProfileSelected { profile_id }),
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
            CoreAgentCommand::SetSkillCatalog { catalog } => {
                require_open(state)?;
                require_no_active_or_queued_work(
                    state,
                    "skill catalog can only change while no run is active or queued",
                )?;

                Ok(vec![CoreAgentEventProposal::new(
                    CoreAgentJoins::default(),
                    CoreAgentEventKind::Skill(crate::SkillEvent::CatalogSet { catalog }),
                )])
            }
            CoreAgentCommand::SetSkillActivations { activations } => {
                require_open(state)?;
                require_no_active_or_queued_work(
                    state,
                    "skill activations can only change while no run is active or queued",
                )?;
                validate_activations(&activations).map_err(command_rejection_from_domain)?;

                Ok(vec![CoreAgentEventProposal::new(
                    CoreAgentJoins::default(),
                    CoreAgentEventKind::Skill(crate::SkillEvent::ActivationsSet { activations }),
                )])
            }
        }
    }
}

fn require_no_active_or_queued_work(
    state: &CoreAgentState,
    message: &'static str,
) -> Result<(), CommandError> {
    if state.runs.active.is_some() || !state.runs.queued.is_empty() {
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
