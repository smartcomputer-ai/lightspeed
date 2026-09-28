use super::*;

/// Admit a batch of client admissions against the live drive, appending the
/// resulting events. Used by the outer loop and from inside
/// the drive loop at every action boundary and while an activity is in
/// flight, so cancel/steer/queue land promptly instead of after the run.
pub(super) async fn admit_admissions(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    admissions: Vec<SessionAdmission>,
) -> anyhow::Result<()> {
    let mut admissions = admissions.into_iter();
    while let Some(admission) = admissions.next() {
        let mut observed_tools = None;
        let admission = match admission {
            SessionAdmission::Operation(request) => {
                preparation::process_operation(ctx, drive, request).await?;
                continue;
            }
            SessionAdmission::Core(admission) => admission,
            SessionAdmission::PreparedRun { admission, result } => {
                let admission = *admission;
                match result {
                    Ok(prepared) => observed_tools = Some(prepared),
                    Err(error) => {
                        record_admission_failure(
                            ctx,
                            preparation::failure(
                                &admission.command,
                                admission.correlation_token,
                                error,
                            ),
                        );
                        continue;
                    }
                }
                admission
            }
        };
        let correlation_token = admission.correlation_token.clone();
        let command = admission.command;
        if let CoreAgentCommand::ReplaceSessionConfig {
            config,
            expected_revision,
        } = &command
        {
            if let Err(error) = preparation::execute_operation(
                ctx,
                drive,
                crate::SessionOperation::Configure {
                    config: config.clone(),
                    expected_revision: *expected_revision,
                },
            )
            .await?
            {
                record_admission_failure(
                    ctx,
                    preparation::failure(&command, correlation_token, error),
                );
            }
            continue;
        }
        let mut deferred_tools = None;
        if drive.state().lifecycle.status == CoreAgentStatus::Open
            && matches!(command, CoreAgentCommand::RequestRun(_))
            && !preparation::known_submission(drive.state(), &command)
        {
            if !ctx.state(|state| state.ready) {
                let error = ctx
                    .state(|state| state.setup_error.clone())
                    .unwrap_or_else(|| {
                        api::AgentApiError::rejected("session setup has not completed")
                    });
                record_admission_failure(
                    ctx,
                    preparation::failure(&command, correlation_token, error),
                );
                continue;
            }
            let Some(prepared) = observed_tools else {
                preparation::begin_run_preparation(
                    ctx,
                    drive,
                    AgentAdmission {
                        command,
                        correlation_token,
                    },
                );
                let remaining = admissions.collect::<Vec<_>>();
                ctx.state_mut(|state| {
                    state.pending_admissions.splice(0..0, remaining);
                });
                break;
            };
            let result = async {
                if !preparation::preparation_matches(
                    &prepared,
                    drive.state(),
                    ctx.state(|state| state.universe_id).unwrap(),
                ) {
                    return Err(api::AgentApiError::conflict(
                        "configuration changed during tool preparation",
                    ));
                }
                if turn_in_flight(drive.state()) {
                    deferred_tools = Some(prepared);
                } else {
                    preparation::publish_tools(ctx, drive, prepared).await?;
                }
                if should_refresh_runtime_projection_before_admitting(drive.state(), &command) {
                    refresh_runtime_projection_before_run(ctx, drive)
                        .await
                        .map_err(|e| api::AgentApiError::internal(e.to_string()))?;
                }
                Ok::<(), api::AgentApiError>(())
            }
            .await;
            if let Err(error) = result {
                record_admission_failure(
                    ctx,
                    preparation::failure(&command, correlation_token, error),
                );
                continue;
            }
        }
        let prepared_admission = AgentAdmission {
            command: command.clone(),
            correlation_token: correlation_token.clone(),
        };
        match admit_and_append_command(ctx, drive, command, correlation_token).await? {
            CommandAdmissionResult::Accepted => {
                if let Some(prepared) = deferred_tools {
                    let run_id = drive
                        .state()
                        .runs
                        .queued
                        .last()
                        .expect("accepted queued run")
                        .run_id;
                    ctx.state_mut(|state| {
                        state.pending_toolsets.push(preparation::PendingToolset {
                            prepared,
                            run_id,
                            admission: prepared_admission,
                        })
                    });
                }
            }
            CommandAdmissionResult::Rejected(failure) => record_admission_failure(ctx, failure),
        }
    }
    Ok(())
}

/// Take and admit the pending admissions that may land right now against
/// the live drive. Returns whether anything was admitted (accepted or
/// rejected). Two classes are held back, in order, for a later drain:
///
/// - everything while a standalone context compaction is pending (run
///   requests would be rejected against that transient state; compaction
///   only runs while no run is active, so nothing time-critical waits);
/// - context/config/tool mutations while a turn's generation is in flight.
///   That turn's request is frozen at its planned revisions and the runtime
///   re-derives it from state, so those revisions must not move until the
///   turn completes. Run control (cancel, steer, queue, promise and
///   workflow-tool facts) carries no such revision and lands immediately.
pub(super) async fn drain_pending_admissions(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
) -> anyhow::Result<bool> {
    if drive.state().context.pending_compaction {
        return Ok(false);
    }
    let turn_in_flight = turn_in_flight(drive.state());
    let admissions = ctx.state_mut(|state| {
        if state.run_preparation.is_some() {
            let (now, later) = std::mem::take(&mut state.pending_admissions)
                .into_iter()
                .partition(preparation::admission_can_pass_preparation);
            state.pending_admissions = later;
            return now;
        }
        if !turn_in_flight {
            return std::mem::take(&mut state.pending_admissions);
        }
        let (now, later): (Vec<_>, Vec<_>) = std::mem::take(&mut state.pending_admissions)
            .into_iter()
            .partition(|admission| admission.admissible_during_turn());
        state.pending_admissions = later;
        now
    });
    if admissions.is_empty() {
        return Ok(false);
    }
    admit_admissions(ctx, drive, admissions).await?;
    Ok(true)
}

/// Pending admissions that `drain_pending_admissions` would admit now.
pub(super) fn has_admissible_admissions(state: &AgentSessionWorkflow) -> bool {
    if state.core_state.context.pending_compaction {
        return false;
    }
    if state.run_preparation.is_some() {
        return state
            .pending_admissions
            .iter()
            .any(preparation::admission_can_pass_preparation);
    }
    if !turn_in_flight(&state.core_state) {
        return !state.pending_admissions.is_empty();
    }
    state
        .pending_admissions
        .iter()
        .any(|admission| admission.admissible_during_turn())
}

pub(super) fn turn_in_flight(state: &CoreAgentState) -> bool {
    state
        .runs
        .active
        .as_ref()
        .is_some_and(|run| run.active_turn_id.is_some())
}

/// Commands that do not move the config/context/toolset revisions an
/// in-flight turn was planned against.
pub(super) fn admissible_during_turn(command: &CoreAgentCommand) -> bool {
    matches!(
        command,
        CoreAgentCommand::CancelRun { .. }
            | CoreAgentCommand::ForceCancelRun { .. }
            | CoreAgentCommand::RequestRunSteering { .. }
            | CoreAgentCommand::DecideApproval(_)
            | CoreAgentCommand::RequestRun(_)
            | CoreAgentCommand::ResolvePromise { .. }
            | CoreAgentCommand::FailWorkflowToolDelivery { .. }
            | CoreAgentCommand::FailWorkflowToolStart { .. }
            | CoreAgentCommand::ResumeToolBatch(_)
            | CoreAgentCommand::CloseSession { .. }
    )
}

pub(super) fn should_refresh_runtime_projection_before_admitting(
    state: &CoreAgentState,
    command: &CoreAgentCommand,
) -> bool {
    matches!(command, CoreAgentCommand::RequestRun(_))
        && state.runs.active.is_none()
        && state.runs.queued.is_empty()
}

pub(super) async fn refresh_runtime_projection_before_run(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
) -> anyhow::Result<()> {
    let request = runtime_projection_request(drive.session_id(), drive.state());
    let commands = prepare_runtime_projection(ctx, drive, request).await?;
    for command in commands {
        match admit_and_append_command(ctx, drive, command, None).await? {
            CommandAdmissionResult::Accepted => {}
            CommandAdmissionResult::Rejected(failure) => {
                anyhow::bail!("run context refresh command rejected: {}", failure.message)
            }
        }
    }
    Ok(())
}

pub(super) fn runtime_projection_request(
    session_id: &SessionId,
    state: &CoreAgentState,
) -> RuntimeProjectionRefreshActivityRequest {
    let vfs = state
        .lifecycle
        .config
        .as_ref()
        .and_then(|config| config.features.vfs.as_ref());
    let vfs_catalog_enabled = vfs.is_some();
    let vfs_prompts_enabled = vfs.is_some_and(|vfs| vfs.prompts.is_some());
    let vfs_prompt_roots = vfs
        .and_then(|vfs| vfs.prompts.as_ref())
        .and_then(|prompts| prompts.roots.clone());
    let vfs_skills = vfs.and_then(|vfs| vfs.skills.clone());
    RuntimeProjectionRefreshActivityRequest {
        environments: state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.environments.clone()),
        active_environment_id: state.environment.active_environment_id.clone(),
        session_id: session_id.clone(),
        workspace_attachments: vfs.map(|vfs| vfs.workspaces.clone()).unwrap_or_default(),
        vfs_catalog_enabled,
        vfs_prompts_enabled,
        vfs_prompt_roots,
        active_instruction_inputs: active_instruction_inputs(state),
        vfs_skills,
        active_catalogs: engine::current_catalog_inputs(state),
        subagents: state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.subagents.clone()),
    }
}

pub(super) async fn prepare_runtime_projection(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    request: RuntimeProjectionRefreshActivityRequest,
) -> anyhow::Result<Vec<CoreAgentCommand>> {
    let activity_ctx = ctx.clone();
    let activity = activity_ctx.start_activity(
        WorkflowActivities::runtime_projection_refresh,
        request,
        preparation::activity_options(),
    );
    let result = preparation::await_activity(ctx, drive, activity)
        .await?
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    Ok(result.commands)
}

pub(super) fn active_instruction_inputs(
    state: &CoreAgentState,
) -> BTreeMap<ContextEntryKey, ContextEntryInput> {
    state
        .context
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, ContextEntryKind::Instructions))
        .filter_map(|entry| {
            let key = entry.key.clone()?;
            (key.as_str() == "instructions" || key.as_str().starts_with("instructions.")).then(
                || {
                    (
                        key,
                        ContextEntryInput {
                            kind: entry.kind.clone(),
                            content: entry.content.clone(),
                            preview: entry.preview.clone(),
                            origin: entry.origin.clone(),
                            provenance_ref: entry.provenance_ref.clone(),
                            token_estimate: entry.token_estimate.clone(),
                        },
                    )
                },
            )
        })
        .collect()
}
