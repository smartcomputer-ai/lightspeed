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
        let mut command = admission.command;
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
        if observed_tools.is_none() && command_needs_input_preprocessing(&command) {
            let session_id = drive.session_id().clone();
            match preprocess_input_entries(ctx, session_id, command).await? {
                RunInputPreprocessResult::Succeeded { command: rewritten } => command = *rewritten,
                RunInputPreprocessResult::Failed { failure } => {
                    record_admission_failure(
                        ctx,
                        failure.with_correlation_token(correlation_token),
                    );
                    continue;
                }
            }
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

enum RunInputPreprocessResult {
    Succeeded { command: Box<CoreAgentCommand> },
    Failed { failure: AgentAdmissionFailure },
}

pub(super) fn command_needs_input_preprocessing(command: &CoreAgentCommand) -> bool {
    match command {
        CoreAgentCommand::RequestRun(request) => request.source.input().iter().any(is_audio_input),
        CoreAgentCommand::UpsertContext { entry, .. } => is_audio_input(entry),
        _ => false,
    }
}

fn is_audio_input(input: &ContextEntryInput) -> bool {
    input
        .content
        .media_type
        .as_deref()
        .map(|mime| mime.trim().to_ascii_lowercase().starts_with("audio/"))
        .unwrap_or(false)
}

async fn preprocess_input_entries(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    session_id: SessionId,
    command: CoreAgentCommand,
) -> anyhow::Result<RunInputPreprocessResult> {
    let (submission_id, input, rebuild) = match command {
        CoreAgentCommand::RequestRun(request) => {
            let engine::RunRequestSource::Input { input } = request.source;
            (
                request.submission_id.clone(),
                input,
                InputPreprocessRebuild::RequestRun {
                    submission_id: request.submission_id,
                    run_config: request.run_config,
                    notify_on_terminal: request.notify_on_terminal,
                },
            )
        }
        CoreAgentCommand::UpsertContext {
            expected_revision,
            key,
            entry,
        } => (
            None,
            vec![entry],
            InputPreprocessRebuild::UpsertContext {
                expected_revision,
                key,
            },
        ),
        command => {
            return Ok(RunInputPreprocessResult::Succeeded {
                command: Box::new(command),
            });
        }
    };

    let result = ctx
        .start_activity(
            WorkflowActivities::preprocess_run_input,
            PreprocessRunInputActivityRequest { session_id, input },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    match result.outcome {
        PreprocessRunInputOutcome::Succeeded { input } => Ok(RunInputPreprocessResult::Succeeded {
            command: Box::new(rebuild.rebuild(input)?),
        }),
        PreprocessRunInputOutcome::Failed { failure } => Ok(RunInputPreprocessResult::Failed {
            failure: preprocess_failure_to_admission_failure(submission_id, failure),
        }),
    }
}

enum InputPreprocessRebuild {
    RequestRun {
        submission_id: Option<SubmissionId>,
        run_config: RunConfig,
        notify_on_terminal: Vec<engine::RunTerminalNotifyIntent>,
    },
    UpsertContext {
        expected_revision: Option<u64>,
        key: ContextEntryKey,
    },
}

impl InputPreprocessRebuild {
    fn rebuild(self, input: Vec<ContextEntryInput>) -> anyhow::Result<CoreAgentCommand> {
        match self {
            Self::RequestRun {
                submission_id,
                run_config,
                notify_on_terminal,
            } => Ok(CoreAgentCommand::RequestRun(engine::RunRequestCommand {
                notify_on_terminal,
                submission_id,
                source: engine::RunRequestSource::Input { input },
                run_config,
            })),
            Self::UpsertContext {
                expected_revision,
                key,
            } => {
                let mut input = input;
                let Some(entry) = input.pop() else {
                    anyhow::bail!("preprocessed context append returned no entry");
                };
                if !input.is_empty() {
                    anyhow::bail!("preprocessed context append returned multiple entries");
                }
                Ok(CoreAgentCommand::UpsertContext {
                    expected_revision,
                    key,
                    entry,
                })
            }
        }
    }
}

pub(super) fn preprocess_failure_to_admission_failure(
    submission_id: Option<SubmissionId>,
    failure: PreprocessRunInputFailure,
) -> AgentAdmissionFailure {
    AgentAdmissionFailure {
        preparation_error: None,
        submission_id,
        correlation_token: None,
        kind: match failure.kind {
            PreprocessRunInputFailureKind::UnsupportedAudioMime => {
                AgentAdmissionFailureKind::UnsupportedAudioMime
            }
            PreprocessRunInputFailureKind::AudioBlobMissing => {
                AgentAdmissionFailureKind::AudioBlobMissing
            }
            PreprocessRunInputFailureKind::AudioBlobTooLarge => {
                AgentAdmissionFailureKind::AudioBlobTooLarge
            }
            PreprocessRunInputFailureKind::AudioDurationTooLong => {
                AgentAdmissionFailureKind::AudioDurationTooLong
            }
            PreprocessRunInputFailureKind::TranscoderUnavailable => {
                AgentAdmissionFailureKind::TranscoderUnavailable
            }
            PreprocessRunInputFailureKind::TranscodeFailure => {
                AgentAdmissionFailureKind::TranscodeFailure
            }
            PreprocessRunInputFailureKind::TranscriptionFailure => {
                AgentAdmissionFailureKind::TranscriptionFailure
            }
        },
        message: failure.message,
        rejection: None,
    }
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
        workspace_links: vfs
            .map(|vfs| vfs.workspace_links.clone())
            .unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_preprocess_rebuild_preserves_expected_context_revision() {
        let key = ContextEntryKey::new("client.audio");
        let entry = ContextEntryInput {
            kind: engine::ContextEntryKind::ProviderOpaque,
            content: engine::ContentRef {
                content_ref: BlobRef::from_bytes(b"transcribed"),
                media_type: Some("application/json".to_owned()),
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        };

        let command = InputPreprocessRebuild::UpsertContext {
            expected_revision: Some(7),
            key: key.clone(),
        }
        .rebuild(vec![entry.clone()])
        .expect("rebuild upsert");

        assert_eq!(
            command,
            CoreAgentCommand::UpsertContext {
                expected_revision: Some(7),
                key,
                entry,
            }
        );
    }
}
