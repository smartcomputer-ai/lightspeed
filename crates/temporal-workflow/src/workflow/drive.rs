use super::*;

pub(super) enum CommandAdmissionResult {
    Accepted,
    Rejected(AgentAdmissionFailure),
}

pub(super) async fn append_command(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    command: CoreAgentCommand,
) -> anyhow::Result<()> {
    match admit_and_append_command(ctx, drive, command, None).await? {
        CommandAdmissionResult::Accepted => Ok(()),
        CommandAdmissionResult::Rejected(failure) => {
            anyhow::bail!("workflow setup command rejected: {}", failure.message)
        }
    }
}

pub(super) async fn admit_and_append_command(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    command: CoreAgentCommand,
    context_key: Option<ContextEntryKey>,
) -> anyhow::Result<CommandAdmissionResult> {
    let submission_id = command_submission_id(&command);
    let action = match drive.admit_command(command, workflow_time_ms(ctx)) {
        Ok(action) => action,
        Err(CoreAgentDriveError::Command(CommandError::Rejected(rejection))) => {
            return Ok(CommandAdmissionResult::Rejected(AgentAdmissionFailure {
                submission_id,
                context_key,
                kind: AgentAdmissionFailureKind::RejectedCommand,
                message: rejection.to_string(),
            }));
        }
        Err(error) => return Err(anyhow::anyhow!("{error}")),
    };
    match action {
        CoreAgentAction::AppendEvents {
            expected_head,
            events,
        } => {
            append_events(ctx, drive, expected_head, events).await?;
            Ok(CommandAdmissionResult::Accepted)
        }
        CoreAgentAction::Idle | CoreAgentAction::Closed => Ok(CommandAdmissionResult::Accepted),
        other => anyhow::bail!("command admission emitted unexpected action: {other:?}"),
    }
}

pub(super) fn command_submission_id(command: &CoreAgentCommand) -> Option<SubmissionId> {
    match command {
        CoreAgentCommand::RequestRun(request) => request.submission_id.clone(),
        _ => None,
    }
}

pub(super) async fn process_pending_tool_batch_resumes(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> anyhow::Result<()> {
    let resumes = ctx.state_mut(|state| std::mem::take(&mut state.pending_tool_batch_resumes));
    if resumes.is_empty() {
        return Ok(());
    }
    let mut drive = drive_from_state(ctx)?;
    for resume in resumes {
        let command = CoreAgentCommand::ResumeToolBatch {
            batch_id: resume.batch_id,
            result: resume.result,
        };
        match admit_and_append_command(ctx, &mut drive, command, None).await? {
            CommandAdmissionResult::Accepted => {}
            CommandAdmissionResult::Rejected(failure) => {
                anyhow::bail!(
                    "pending tool batch resume was rejected: {}",
                    failure.message
                )
            }
        }
    }
    drive_until_idle(ctx, args, &mut drive).await
}

pub(super) async fn drive_until_idle(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
    drive: &mut CoreAgentDrive,
) -> anyhow::Result<()> {
    let max_steps = args
        .max_steps_per_input
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_MAX_STEPS_PER_INPUT);
    drive.reset_steps();
    let mut action = drive.next_action(workflow_time_ms(ctx), max_steps)?;
    loop {
        match action {
            CoreAgentAction::AppendEvents {
                expected_head,
                events,
            } => {
                append_events(ctx, drive, expected_head, events).await?;
                action = drive.next_action(workflow_time_ms(ctx), max_steps)?;
            }
            CoreAgentAction::GenerateLlm { request } => {
                let result = call_llm_generate(ctx, request).await?;
                action = drive.resume_generation(result, workflow_time_ms(ctx))?;
            }
            CoreAgentAction::CompactContext { request } => {
                let result = call_context_compact(ctx, request).await?;
                action = drive.resume_context_compaction(result, workflow_time_ms(ctx))?;
            }
            CoreAgentAction::InvokeTools { request } => {
                let outcome = call_tool_invoke_batch(ctx, request).await?;
                action = drive.resume_tool_batch_outcome(outcome, workflow_time_ms(ctx))?;
            }
            CoreAgentAction::Idle | CoreAgentAction::Closed => {
                maybe_close_on_terminal(ctx, args, drive).await?;
                return Ok(());
            }
            CoreAgentAction::StepLimitReached => {
                // Deferred for G4: step limits can happen after partial run progress.
                // Keep treating them as workflow failures until resume semantics are explicit.
                anyhow::bail!("Agent drive step limit reached: max_steps={max_steps}");
            }
        }
    }
}

async fn maybe_close_on_terminal(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
    drive: &mut CoreAgentDrive,
) -> anyhow::Result<()> {
    if !should_close_on_terminal(args, drive.state()) {
        return Ok(());
    }
    match admit_and_append_command(ctx, drive, CoreAgentCommand::CloseSession, None).await? {
        CommandAdmissionResult::Accepted => Ok(()),
        CommandAdmissionResult::Rejected(failure) => {
            anyhow::bail!(
                "close_on_terminal CloseSession was rejected: {}",
                failure.message
            )
        }
    }
}

pub(super) fn should_close_on_terminal(args: &AgentSessionArgs, state: &CoreAgentState) -> bool {
    args.close_on_terminal
        && state.lifecycle.status == CoreAgentStatus::Open
        && !state.runs.completed.is_empty()
        && state.runs.active.is_none()
        && state.runs.queued.is_empty()
        && !state.context.pending_compaction
}

async fn append_events(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    expected_head: Option<SessionPosition>,
    events: Vec<engine::storage::UncommittedStoredEvent>,
) -> anyhow::Result<Vec<CoreAgentEntry>> {
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let appended = ctx
        .start_activity(
            WorkflowActivities::append_events,
            AppendEventsRequest {
                session_id: drive.session_id().clone(),
                expected_head,
                events,
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let entries = drive.resume_appended(appended.entries)?;
    let deferred_waits = entries
        .iter()
        .filter_map(|entry| wait_directive_for_event(&entry.event).transpose())
        .collect::<anyhow::Result<Vec<_>>>()?;
    let deferred_environment_job_waits = entries
        .iter()
        .filter_map(|entry| job_waits::directive_for_event(&entry.event).transpose())
        .collect::<anyhow::Result<Vec<_>>>()?;
    ctx.state_mut(|state| -> anyhow::Result<()> {
        apply_entries(&mut state.core_state, &entries, &mut state.run_submissions)?;
        state.queue_terminal_notifications_for_entries(&entries);
        state.head = appended.head;
        state.last_error = None;
        Ok(())
    })?;
    for wait in deferred_waits {
        install_deferred_wait(ctx, wait).await?;
    }
    for wait in deferred_environment_job_waits {
        job_waits::install(ctx, wait);
    }
    Ok(entries)
}

pub(super) fn drive_from_state(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<CoreAgentDrive> {
    let (session_id, core_state, head) = ctx.state(|state| {
        (
            state.session_id.clone(),
            state.core_state.clone(),
            state.head.clone(),
        )
    });
    let Some(session_id) = session_id else {
        anyhow::bail!("missing initialized agent session id");
    };
    Ok(CoreAgentDrive::from_replayed(session_id, core_state, head))
}

fn apply_entries(
    state: &mut CoreAgentState,
    entries: &[CoreAgentEntry],
    run_submissions: &mut BTreeMap<u64, Option<SubmissionId>>,
) -> anyhow::Result<()> {
    for entry in entries {
        if let CoreAgentEvent::Run(RunEvent::Accepted(accepted)) = &entry.event {
            run_submissions.insert(accepted.run_id.as_u64(), accepted.submission_id.clone());
        }
        engine::apply_event(state, entry)?;
    }
    Ok(())
}
