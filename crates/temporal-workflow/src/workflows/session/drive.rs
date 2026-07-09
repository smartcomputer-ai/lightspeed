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
        CoreAgentCommand::SubmitMessage(message) => message.submission_id.clone(),
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
        let command = CoreAgentCommand::ResumeAwait(resume.command);
        match admit_and_append_command(ctx, &mut drive, command, None).await? {
            CommandAdmissionResult::Accepted => {}
            CommandAdmissionResult::Rejected(failure) => {
                // A rejected resume must never fail the session loop: that
                // turns one bad batch result into a permanently wedged
                // workflow (the 2026-07-06 incident shape). Record it and
                // continue; if the run is now stuck in `cancelling`, the
                // watchdog forces it terminal.
                record_admission_failure(ctx, failure);
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
    match admit_and_append_command(
        ctx,
        drive,
        CoreAgentCommand::CloseSession { force: false },
        None,
    )
    .await?
    {
        CommandAdmissionResult::Accepted => Ok(()),
        CommandAdmissionResult::Rejected(failure) => {
            record_admission_failure(ctx, failure);
            Ok(())
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
        && !state
            .promises
            .pending()
            .any(|promise| promise.scope == engine::PromiseScope::Session)
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
    ctx.state_mut(|state| -> anyhow::Result<()> {
        apply_entries(&mut state.core_state, &entries, &mut state.run_submissions)?;
        state.queue_promise_notifications_for_entries(&entries);
        state.queue_promise_cancellations_for_entries(&entries);
        state.head = appended.head;
        state.last_error = None;
        Ok(())
    })?;
    queue_detached_promise_followups(ctx, &entries).await?;
    Ok(entries)
}

#[derive(Clone, Debug)]
struct DetachedPromiseFollowup {
    promise_id: engine::PromiseId,
    status: &'static str,
    content_ref: Option<BlobRef>,
}

async fn queue_detached_promise_followups(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    entries: &[CoreAgentEntry],
) -> anyhow::Result<()> {
    let followups = ctx.state(|state| {
        if state.core_state.lifecycle.status != CoreAgentStatus::Open {
            return Vec::new();
        }
        entries
            .iter()
            .filter_map(|entry| detached_promise_followup_for_entry(state, entry))
            .collect::<Vec<_>>()
    });

    for followup in followups {
        let summary_ref = put_detached_followup_blob(
            ctx,
            detached_promise_followup_summary(&followup).into_bytes(),
        )
        .await?;
        let mut input = vec![workflow_user_message_input(
            summary_ref,
            Some(format!(
                "Detached promise {} {}",
                followup.promise_id, followup.status
            )),
        )];
        if let Some(content_ref) = followup.content_ref {
            input.push(workflow_user_message_input(
                content_ref,
                Some(format!(
                    "Detached promise {} {} content",
                    followup.promise_id, followup.status
                )),
            ));
        }
        let submission_id = detached_promise_submission_id(&followup.promise_id);
        ctx.state_mut(|state| {
            state.pending_admissions.push(AgentAdmission {
                command: CoreAgentCommand::SubmitMessage(engine::SubmitMessageCommand {
                    submission_id: Some(submission_id),
                    input,
                }),
                context_key: None,
            });
        });
    }
    Ok(())
}

fn detached_promise_followup_for_entry(
    state: &AgentSessionWorkflow,
    entry: &CoreAgentEntry,
) -> Option<DetachedPromiseFollowup> {
    let (promise_id, status, content_ref) = match &entry.event {
        CoreAgentEvent::Promise(engine::PromiseEvent::Resolved {
            promise_id,
            payload_ref,
        }) => (promise_id, "resolved", payload_ref.clone()),
        CoreAgentEvent::Promise(engine::PromiseEvent::Failed {
            promise_id,
            error_ref,
        }) => (promise_id, "failed", error_ref.clone()),
        _ => return None,
    };
    let promise = state.core_state.promises.promises.get(promise_id)?;
    if promise.scope != engine::PromiseScope::Session || !promise.status.is_terminal() {
        return None;
    }
    if awaits::parked_await(&state.core_state)
        .is_some_and(|parked| parked.spec.promise_ids.iter().any(|id| id == promise_id))
    {
        return None;
    }
    Some(DetachedPromiseFollowup {
        promise_id: promise_id.clone(),
        status,
        content_ref,
    })
}

fn detached_promise_followup_summary(followup: &DetachedPromiseFollowup) -> String {
    match followup.content_ref.as_ref() {
        Some(_) => format!(
            "Detached promise {} {}. The promise content is attached as the next user message.",
            followup.promise_id, followup.status
        ),
        None => format!(
            "Detached promise {} {} without attached content.",
            followup.promise_id, followup.status
        ),
    }
}

async fn put_detached_followup_blob(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    bytes: Vec<u8>,
) -> anyhow::Result<BlobRef> {
    ctx.start_activity(
        WorkflowActivities::put_blob,
        PutBlobRequest { bytes },
        activity_options(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))
}

fn detached_promise_submission_id(promise_id: &engine::PromiseId) -> SubmissionId {
    let digest = BlobRef::from_bytes(format!("detached_promise:{promise_id}").as_bytes());
    let suffix = digest
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(digest.as_str())
        .chars()
        .take(32)
        .collect::<String>();
    SubmissionId::new(format!("detached_promise_{suffix}"))
}

fn workflow_user_message_input(content_ref: BlobRef, preview: Option<String>) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content_ref,
        media_type: None,
        preview,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_session_promise_resolution_produces_followup_candidate() {
        let promise_id = engine::PromiseId::new("promise_detached");
        let payload_ref = BlobRef::from_bytes(b"child output");
        let mut workflow = AgentSessionWorkflow::default();
        workflow.core_state.lifecycle.status = CoreAgentStatus::Open;
        workflow.core_state.promises.promises.insert(
            promise_id.clone(),
            engine::Promise {
                promise_id: promise_id.clone(),
                source: engine::PromiseSource::Run {
                    target_session_id: "child".to_owned(),
                    target_run_id: 1,
                },
                scope: engine::PromiseScope::Session,
                status: engine::PromiseStatus::Resolved,
                payload_ref: Some(payload_ref.clone()),
                error_ref: None,
                deadline_ms: None,
            },
        );
        let entry = CoreAgentEntry {
            position: SessionPosition {
                seq: engine::EventSeq::new(1),
            },
            observed_at_ms: 1,
            joins: Default::default(),
            event: CoreAgentEvent::Promise(engine::PromiseEvent::Resolved {
                promise_id: promise_id.clone(),
                payload_ref: Some(payload_ref.clone()),
            }),
        };

        let followup =
            detached_promise_followup_for_entry(&workflow, &entry).expect("followup candidate");

        assert_eq!(followup.promise_id, promise_id);
        assert_eq!(followup.status, "resolved");
        assert_eq!(followup.content_ref, Some(payload_ref));
    }
}
