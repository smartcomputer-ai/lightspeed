use super::*;

pub(super) async fn process_admissions(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
    admissions: Vec<AgentAdmission>,
) -> anyhow::Result<()> {
    let mut drive = drive_from_state(ctx)?;
    for admission in admissions {
        let correlation_token = admission.correlation_token.clone();
        let mut command = admission.command;
        if command_needs_input_preprocessing(&command) {
            let session_id = drive.session_id().clone();
            match preprocess_input_entries(ctx, session_id, command).await? {
                RunInputPreprocessResult::Succeeded { command: rewritten } => {
                    command = rewritten;
                }
                RunInputPreprocessResult::Failed { failure } => {
                    record_admission_failure(
                        ctx,
                        failure.with_correlation_token(correlation_token.clone()),
                    );
                    continue;
                }
            }
        }
        if should_refresh_runtime_projection_before_admitting(drive.state(), &command) {
            refresh_runtime_projection_before_run(ctx, &mut drive).await?;
        }
        match admit_and_append_command(ctx, &mut drive, command, correlation_token).await? {
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

pub(super) fn command_needs_input_preprocessing(command: &CoreAgentCommand) -> bool {
    match command {
        CoreAgentCommand::RequestRun(request) => request.source.input().iter().any(is_audio_input),
        CoreAgentCommand::UpsertContext { entry, .. } => is_audio_input(entry),
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

async fn preprocess_input_entries(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    session_id: SessionId,
    command: CoreAgentCommand,
) -> anyhow::Result<RunInputPreprocessResult> {
    let (submission_id, input, rebuild) = match command {
        CoreAgentCommand::RequestRun(request) => match request.source {
            engine::RunRequestSource::Input { input } => (
                request.submission_id.clone(),
                input,
                InputPreprocessRebuild::RequestRun {
                    submission_id: request.submission_id,
                    run_config: request.run_config,
                    notify_on_terminal: request.notify_on_terminal,
                },
            ),
            engine::RunRequestSource::Context { .. } => {
                return Ok(RunInputPreprocessResult::Succeeded {
                    command: CoreAgentCommand::RequestRun(request),
                });
            }
        },
        CoreAgentCommand::SubmitMessage(message) => (
            message.submission_id.clone(),
            message.input,
            InputPreprocessRebuild::SubmitMessage {
                submission_id: message.submission_id,
            },
        ),
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
        command => return Ok(RunInputPreprocessResult::Succeeded { command }),
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
            command: rebuild.rebuild(input)?,
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
    SubmitMessage {
        submission_id: Option<SubmissionId>,
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
            Self::SubmitMessage { submission_id } => Ok(CoreAgentCommand::SubmitMessage(
                engine::SubmitMessageCommand {
                    submission_id,
                    input,
                },
            )),
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

fn should_refresh_runtime_projection_before_admitting(
    state: &CoreAgentState,
    command: &CoreAgentCommand,
) -> bool {
    matches!(command, CoreAgentCommand::RequestRun(_))
        && state.runs.active.is_none()
        && state.runs.queued.is_empty()
}

async fn refresh_runtime_projection_before_run(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
) -> anyhow::Result<()> {
    let vfs = drive
        .state()
        .lifecycle
        .config
        .as_ref()
        .and_then(|config| config.features.vfs.as_ref());
    let vfs_catalog_enabled = vfs.is_some();
    let environment_catalog_enabled = drive
        .state()
        .lifecycle
        .config
        .as_ref()
        .is_some_and(|config| config.features.environments.is_some());
    let vfs_skills_enabled = vfs.is_some_and(|vfs| vfs.skills.is_some());
    let vfs_skill_roots = vfs
        .and_then(|vfs| vfs.skills.as_ref())
        .and_then(|skills| skills.roots.clone());
    let result = ctx
        .start_activity(
            WorkflowActivities::runtime_projection_refresh,
            RuntimeProjectionRefreshActivityRequest {
                session_id: drive.session_id().clone(),
                vfs_catalog_enabled,
                environment_catalog_enabled,
                vfs_skills_enabled,
                vfs_skill_roots,
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
        match admit_and_append_command(ctx, drive, command, None).await? {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_preprocess_rebuild_preserves_expected_context_revision() {
        let key = ContextEntryKey::new("client.audio");
        let entry = ContextEntryInput {
            kind: engine::ContextEntryKind::ProviderOpaque,
            content_ref: BlobRef::from_bytes(b"transcribed"),
            media_type: Some("application/json".to_owned()),
            preview: None,
            provider_kind: None,
            provider_item_id: None,
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
