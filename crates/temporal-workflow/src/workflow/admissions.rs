use super::*;

pub(super) async fn process_admissions(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
    admissions: Vec<AgentAdmission>,
) -> anyhow::Result<()> {
    let mut drive = drive_from_state(ctx)?;
    for admission in admissions {
        let mut command = match CoreAgentCodec.decode_command(&admission.command) {
            Ok(command) => command,
            Err(error) => {
                record_admission_failure(
                    ctx,
                    AgentAdmissionFailure {
                        submission_id: None,
                        kind: AgentAdmissionFailureKind::InvalidCommand,
                        message: format!("invalid CoreAgent command admission: {error}"),
                    },
                );
                continue;
            }
        };
        if command_needs_run_input_preprocessing(&command) {
            let session_id = drive.session_id().clone();
            match preprocess_run_input(ctx, session_id, command).await? {
                RunInputPreprocessResult::Succeeded { command: rewritten } => {
                    command = rewritten;
                }
                RunInputPreprocessResult::Failed { failure } => {
                    record_admission_failure(ctx, failure);
                    continue;
                }
            }
        }
        if should_refresh_skill_catalog_before_admitting(drive.state(), &command) {
            refresh_skill_catalog_before_run(ctx, &mut drive).await?;
        }
        match admit_and_append_command(ctx, &mut drive, command).await? {
            CommandAdmissionResult::Accepted => {}
            CommandAdmissionResult::Rejected(failure) => {
                record_admission_failure(ctx, failure);
            }
        }
    }
    drive_until_idle(ctx, args, &mut drive).await
}

enum RunInputPreprocessResult {
    Succeeded { command: CoreAgentCommand },
    Failed { failure: AgentAdmissionFailure },
}

pub(super) fn command_needs_run_input_preprocessing(command: &CoreAgentCommand) -> bool {
    match command {
        CoreAgentCommand::RequestRun { input, .. } => input.iter().any(is_audio_input),
        _ => false,
    }
}

fn is_audio_input(input: &ContextEntryInput) -> bool {
    input
        .media_type
        .as_deref()
        .map(|mime| mime.trim().to_ascii_lowercase().starts_with("audio/"))
        .unwrap_or(false)
}

async fn preprocess_run_input(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    session_id: SessionId,
    command: CoreAgentCommand,
) -> anyhow::Result<RunInputPreprocessResult> {
    let CoreAgentCommand::RequestRun {
        submission_id,
        input,
        run_config,
    } = command
    else {
        return Ok(RunInputPreprocessResult::Succeeded { command });
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
            command: CoreAgentCommand::RequestRun {
                submission_id,
                input,
                run_config,
            },
        }),
        PreprocessRunInputOutcome::Failed { failure } => Ok(RunInputPreprocessResult::Failed {
            failure: preprocess_failure_to_admission_failure(submission_id, failure),
        }),
    }
}

pub(super) fn preprocess_failure_to_admission_failure(
    submission_id: Option<SubmissionId>,
    failure: PreprocessRunInputFailure,
) -> AgentAdmissionFailure {
    AgentAdmissionFailure {
        submission_id,
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
    }
}

fn should_refresh_skill_catalog_before_admitting(
    state: &CoreAgentState,
    command: &CoreAgentCommand,
) -> bool {
    matches!(command, CoreAgentCommand::RequestRun { .. })
        && state.runs.active.is_none()
        && state.runs.queued.is_empty()
}

async fn refresh_skill_catalog_before_run(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
) -> anyhow::Result<()> {
    let result = ctx
        .start_activity(
            WorkflowActivities::skill_catalog_refresh,
            SkillCatalogRefreshActivityRequest {
                session_id: drive.session_id().clone(),
                active_catalog_ref: active_skill_catalog_ref(drive.state()),
                active_vfs_catalog_ref: active_context_ref(
                    drive.state(),
                    VFS_CATALOG_CONTEXT_KEY,
                    ContextEntryKind::VfsCatalog,
                ),
                active_environment_catalog_ref: active_context_ref(
                    drive.state(),
                    ENVIRONMENT_CATALOG_CONTEXT_KEY,
                    ContextEntryKind::EnvironmentCatalog,
                ),
                active_environment_active_ref: active_context_ref(
                    drive.state(),
                    ENVIRONMENT_ACTIVE_CONTEXT_KEY,
                    ContextEntryKind::EnvironmentActive,
                ),
                active_environment_target: drive
                    .state()
                    .tooling
                    .routing
                    .default_targets
                    .get("env")
                    .cloned(),
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    for command in result.commands {
        match admit_and_append_command(ctx, drive, command).await? {
            CommandAdmissionResult::Accepted => {}
            CommandAdmissionResult::Rejected(failure) => {
                anyhow::bail!("run context refresh command rejected: {}", failure.message)
            }
        }
    }
    Ok(())
}

fn active_skill_catalog_ref(state: &CoreAgentState) -> Option<BlobRef> {
    active_context_ref(
        state,
        SKILL_CATALOG_CONTEXT_KEY,
        ContextEntryKind::SkillCatalog,
    )
}

fn active_context_ref(
    state: &CoreAgentState,
    key: &'static str,
    kind: ContextEntryKind,
) -> Option<BlobRef> {
    state
        .context
        .entries
        .iter()
        .find(|entry| {
            entry
                .key
                .as_ref()
                .is_some_and(|entry_key| entry_key.as_str() == key)
                && entry.kind == kind
        })
        .map(|entry| entry.content_ref.clone())
}
