use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DriveOutcome {
    Idle,
    /// History rollover is due, but a transport queue must be drained by the
    /// outer workflow loop before continuation can safely occur.
    YieldForWorkflowWork,
    ContinueAsNew,
}

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
    correlation_token: Option<String>,
) -> anyhow::Result<CommandAdmissionResult> {
    let submission_id = command_submission_id(&command);
    if environment_prompt_publication_is_obsolete(drive.state(), &command)
        || environment_catalog_publication_is_obsolete(drive.state(), &command)
        || vfs_skill_catalog_publication_is_obsolete(drive.state(), &command)
    {
        let rejection = engine::CommandRejection::new(
            engine::CommandRejectionKind::ActiveWork,
            "skill catalog observation no longer matches an idle configured source",
        );
        return Ok(CommandAdmissionResult::Rejected(AgentAdmissionFailure {
            preparation_error: None,
            submission_id,
            correlation_token,
            kind: AgentAdmissionFailureKind::RejectedCommand,
            message: rejection.to_string(),
            rejection: Some(rejection),
        }));
    }
    let action = match drive.admit_command(command, workflow_time_ms(ctx)) {
        Ok(action) => action,
        Err(CoreAgentDriveError::Command(CommandError::Rejected(rejection))) => {
            let message = rejection.to_string();
            return Ok(CommandAdmissionResult::Rejected(AgentAdmissionFailure {
                preparation_error: None,
                submission_id,
                correlation_token,
                kind: AgentAdmissionFailureKind::RejectedCommand,
                message,
                rejection: Some(rejection),
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
) -> anyhow::Result<DriveOutcome> {
    let resumes = ctx.state_mut(|state| std::mem::take(&mut state.pending_tool_batch_resumes));
    if resumes.is_empty() {
        return Ok(DriveOutcome::Idle);
    }
    let mut drive = drive_from_state(ctx)?;
    for resume in resumes {
        let command = CoreAgentCommand::ResumeToolBatch(resume.command);
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
) -> anyhow::Result<DriveOutcome> {
    if let Some(outcome) = history_boundary_outcome(ctx, args) {
        return Ok(outcome);
    }
    if drive.state().lifecycle.status == CoreAgentStatus::Closed {
        ctx.state_mut(preparation::abandon_pending_run);
    }
    preparation::publish_pending_tools(ctx, drive).await?;
    let mut action = drive.next_action_unbounded(workflow_time_ms(ctx))?;
    loop {
        if preparation::publish_pending_tools(ctx, drive).await? {
            action = drive.next_action_unbounded(workflow_time_ms(ctx))?;
        }
        // Client admissions (cancel, steer, queue, context edits) land
        // at every action boundary against the live drive, and the plan is
        // recomputed so a cancel stops the next turn/batch from starting and
        // a steer is part of the next request.
        if admissions::drain_pending_admissions(ctx, drive).await? {
            action = drive.next_action_unbounded(workflow_time_ms(ctx))?;
        }
        match action {
            CoreAgentAction::AppendEvents {
                expected_head,
                events,
            } => {
                append_events(ctx, drive, expected_head, events).await?;
                if let Some(outcome) = history_boundary_outcome(ctx, args) {
                    return Ok(outcome);
                }
                action = drive.next_action_unbounded(workflow_time_ms(ctx))?;
            }
            CoreAgentAction::GenerateLlm { request } => {
                action = match call_llm_generate(ctx, drive, request).await? {
                    control::Raced::Completed(result) => {
                        drive.resume_generation(result, workflow_time_ms(ctx))?
                    }
                    control::Raced::Preempted => {
                        drive.next_action_unbounded(workflow_time_ms(ctx))?
                    }
                };
            }
            CoreAgentAction::CompactContext { request } => {
                let result = call_context_compact(ctx, request).await?;
                action = drive.resume_context_compaction(result, workflow_time_ms(ctx))?;
            }
            CoreAgentAction::InvokeTools { request } => {
                let request = match request.promise_control_argument_request() {
                    Some(argument_request) => {
                        let facts =
                            call_tool_prepare_promise_controls(ctx, argument_request).await?;
                        engine::attach_promise_control_runtime(drive.state(), request, facts)?
                    }
                    None => request,
                };
                action = tool_batches::invoke_tool_batch(ctx, drive, request).await?;
            }
            CoreAgentAction::Idle | CoreAgentAction::Closed => {
                if args.auto_reject_approvals && auto_reject_pending_approvals(ctx, drive).await? {
                    action = drive.next_action_unbounded(workflow_time_ms(ctx))?;
                    continue;
                }
                maybe_close_on_terminal(ctx, args, drive).await?;
                return Ok(DriveOutcome::Idle);
            }
            CoreAgentAction::StepLimitReached => {
                anyhow::bail!("unbounded hosted drive unexpectedly reached a step limit");
            }
        }
    }
}

async fn auto_reject_pending_approvals(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
) -> anyhow::Result<bool> {
    const NOTE: &str = "approval required, but no approval surface reaches this session";
    let Some(run_id) = drive.state().runs.active.as_ref().map(|run| run.run_id) else {
        return Ok(false);
    };
    let pending = drive
        .state()
        .runs
        .active
        .as_ref()
        .expect("active run was resolved above")
        .pending_approvals()
        .map(|record| record.request.clone())
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(false);
    }
    for approval in pending {
        let response = match &approval.continuation {
            engine::ApprovalContinuation::OpenAiMcp {
                provider_request_id,
            } => {
                let response = serde_json::json!({
                    "type": "mcp_approval_response",
                    "approval_request_id": provider_request_id,
                    "approve": false,
                });
                let content_ref =
                    put_detached_followup_blob(ctx, serde_json::to_vec(&response)?).await?;
                Some(ContextEntryInput {
                    kind: ContextEntryKind::McpApprovalResponse {
                        approval_request_id: provider_request_id.clone(),
                        approve: false,
                    },
                    content: engine::ContentRef {
                        content_ref,
                        media_type: Some("application/json".to_owned()),
                        provider_kind: Some("openai.responses.mcp_approval_response".to_owned()),
                    },
                    preview: Some("MCP tool call auto-rejected in unattended session".to_owned()),
                    origin: None,
                    provenance_ref: None,
                    token_estimate: None,
                })
            }
            engine::ApprovalContinuation::NativeMcp { .. } => None,
        };
        match admit_and_append_command(
            ctx,
            drive,
            CoreAgentCommand::DecideApproval(engine::ApprovalDecisionCommand {
                approval_id: approval.approval_id,
                run_id,
                decision: engine::ApprovalDecision::Rejected,
                note: Some(NOTE.to_owned()),
                decided_by: None,
                response,
            }),
            None,
        )
        .await?
        {
            CommandAdmissionResult::Accepted => {}
            CommandAdmissionResult::Rejected(failure) => {
                anyhow::bail!("auto-reject approval command failed: {}", failure.message)
            }
        }
    }
    Ok(true)
}

fn history_boundary_outcome(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> Option<DriveOutcome> {
    if !ctx.patched(wait_loop::ACTIVE_RUN_ROLLOVER_PATCH) {
        // Old executions did not branch here. The patch marker keeps replay
        // deterministic; once execution reaches new history, the SDK records
        // the marker and active-run rollover becomes eligible.
        return None;
    }
    // Pending preparation may need the current turn to finish before it can
    // publish. Let the driver reach that boundary instead of yielding forever.
    if ctx.state(|state| state.run_preparation.is_some() || !state.pending_toolsets.is_empty()) {
        return None;
    }
    history_boundary_outcome_for(
        wait_loop::history_rollover_due(ctx, args),
        ctx.state(wait_loop::workflow_state_allows_continue_as_new),
    )
}

pub(super) fn history_boundary_outcome_for(
    rollover_due: bool,
    continuation_safe: bool,
) -> Option<DriveOutcome> {
    rollover_due.then_some(if continuation_safe {
        DriveOutcome::ContinueAsNew
    } else {
        DriveOutcome::YieldForWorkflowWork
    })
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

pub(super) async fn append_events(
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
        state.queue_emissions_for_entries(&entries)?;
        state.queue_promise_cancellations_for_entries(&entries);
        state.head = appended.head;
        state.execution_has_rollover_checkpoint = true;
        state.last_error = None;
        if state.core_state.lifecycle.status == CoreAgentStatus::Closed {
            preparation::abandon_pending_run(state);
            state.pending_toolsets.clear();
        }
        Ok(())
    })?;
    // Invalidation uses only recorded source identity. It performs no discovery,
    // including when selection changed in a tool result during an active run.
    if let Some(command) = invalid_environment_prompt_command(drive.state()) {
        Box::pin(append_command(ctx, drive, command)).await?;
    }
    if let Some(command) = invalid_environment_catalog_command(drive.state()) {
        Box::pin(append_command(ctx, drive, command)).await?;
    }
    if let Some(command) = invalid_vfs_skill_catalog_command(drive.state()) {
        Box::pin(append_command(ctx, drive, command)).await?;
    }
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
        // A detached promise settling on an idle session wakes it with an
        // ordinary run; the submission id is derived from the promise so a
        // replayed follow-up is a no-op.
        ctx.state_mut(|state| {
            state.queue_admission(AgentAdmission {
                command: CoreAgentCommand::RequestRun(engine::RunRequestCommand {
                    notify_on_terminal: Vec::new(),
                    submission_id: Some(submission_id),
                    source: engine::RunRequestSource::Input { input },
                    run_config: engine::RunConfig::default(),
                }),
                correlation_token: None,
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
    if awaits::parked_tool_batch(&state.core_state).is_some_and(|parked| {
        parked
            .suspension
            .spec()
            .promise_ids
            .iter()
            .any(|id| id == promise_id)
    }) {
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
        content: engine::ContentRef {
            content_ref,
            media_type: None,
            provider_kind: None,
        },
        preview,
        origin: None,
        provenance_ref: None,
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
    let mut reduced = crate::ReducedSession {
        core_state: std::mem::take(state),
        run_submissions: std::mem::take(run_submissions),
        replayed_event_count: 0,
    };
    for entry in entries {
        crate::accumulate_session_entry(&mut reduced, entry)?;
    }
    *state = reduced.core_state;
    *run_submissions = reduced.run_submissions;
    Ok(())
}

fn vfs_skill_discovery_enabled(state: &CoreAgentState) -> bool {
    state
        .lifecycle
        .config
        .as_ref()
        .and_then(|config| config.features.vfs.as_ref())
        .and_then(|feature| feature.skills.as_ref())
        .is_some()
}

pub(super) fn invalid_vfs_skill_catalog_command(
    state: &CoreAgentState,
) -> Option<CoreAgentCommand> {
    let key = ContextEntryKey::new("runtime.catalog.skills.vfs");
    let entry = engine::current_context_entry(state, &key)?;
    (state.lifecycle.status == CoreAgentStatus::Open
        && entry.origin.as_deref() == Some("runtime.vfs.skills")
        && !vfs_skill_discovery_enabled(state))
    .then_some(CoreAgentCommand::RemoveContext {
        expected_revision: None,
        key,
    })
}

pub(super) fn vfs_skill_catalog_publication_is_obsolete(
    state: &CoreAgentState,
    command: &CoreAgentCommand,
) -> bool {
    let CoreAgentCommand::UpsertContext { key, entry, .. } = command else {
        return false;
    };
    key.as_str() == "runtime.catalog.skills.vfs"
        && entry.origin.as_deref() == Some("runtime.vfs.skills")
        && (state.runs.active.is_some()
            || !state.runs.queued.is_empty()
            || !vfs_skill_discovery_enabled(state))
}

const ENVIRONMENT_PROMPT_KEY: &str = "instructions.110.environment";

pub(super) fn invalid_environment_prompt_command(
    state: &CoreAgentState,
) -> Option<CoreAgentCommand> {
    let key = ContextEntryKey::new(ENVIRONMENT_PROMPT_KEY);
    let entry = engine::current_context_entry(state, &key)?;
    let source = entry
        .origin
        .as_deref()?
        .strip_prefix("runtime.environment:")?;
    let enabled = state
        .lifecycle
        .config
        .as_ref()
        .and_then(|config| config.features.environments.as_ref())
        .is_some_and(|feature| feature.prompts.is_some());
    (state.lifecycle.status == CoreAgentStatus::Open
        && (!enabled
            || state
                .environment
                .active_environment_id
                .as_ref()
                .map(|id| id.as_str())
                != Some(source)))
    .then_some(CoreAgentCommand::RemoveContext {
        expected_revision: None,
        key,
    })
}

pub(super) fn environment_prompt_publication_is_obsolete(
    state: &CoreAgentState,
    command: &CoreAgentCommand,
) -> bool {
    let CoreAgentCommand::ReplaceContextPrefix { entries, .. } = command else {
        return false;
    };
    let Some(entry) = entries.get(&ContextEntryKey::new(ENVIRONMENT_PROMPT_KEY)) else {
        return false;
    };
    let Some(source) = entry
        .origin
        .as_deref()
        .and_then(|origin| origin.strip_prefix("runtime.environment:"))
    else {
        return false;
    };
    let enabled = state
        .lifecycle
        .config
        .as_ref()
        .and_then(|config| config.features.environments.as_ref())
        .is_some_and(|feature| feature.prompts.is_some());
    !enabled
        || state
            .environment
            .active_environment_id
            .as_ref()
            .map(|id| id.as_str())
            != Some(source)
        || ((state.runs.active.is_some() || !state.runs.queued.is_empty())
            && engine::current_context_entry(state, &ContextEntryKey::new(ENVIRONMENT_PROMPT_KEY))
                .is_none_or(|current| current.content != entry.content))
}

pub(super) fn invalid_environment_catalog_command(
    state: &CoreAgentState,
) -> Option<CoreAgentCommand> {
    let key = ContextEntryKey::new("runtime.catalog.skills.environment");
    let entry = engine::current_context_entry(state, &key)?;
    let source = entry
        .origin
        .as_deref()?
        .strip_prefix("runtime.environment:")?;
    let enabled = state
        .lifecycle
        .config
        .as_ref()
        .and_then(|config| config.features.environments.as_ref())
        .is_some_and(|feature| feature.skills.is_some());
    (state.lifecycle.status == CoreAgentStatus::Open
        && (!enabled
            || state
                .environment
                .active_environment_id
                .as_ref()
                .map(|id| id.as_str())
                != Some(source)))
    .then_some(CoreAgentCommand::RemoveContext {
        expected_revision: None,
        key,
    })
}

pub(super) fn environment_catalog_publication_is_obsolete(
    state: &CoreAgentState,
    command: &CoreAgentCommand,
) -> bool {
    let CoreAgentCommand::UpsertContext { key, entry, .. } = command else {
        return false;
    };
    if key.as_str() != "runtime.catalog.skills.environment" {
        return false;
    }
    state.runs.active.is_some()
        || !state.runs.queued.is_empty()
        || !state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.environments.as_ref())
            .is_some_and(|feature| feature.skills.is_some())
        || entry
            .origin
            .as_deref()
            .and_then(|origin| origin.strip_prefix("runtime.environment:"))
            != state
                .environment
                .active_environment_id
                .as_ref()
                .map(|id| id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_session_promise_resolution_produces_followup_candidate() {
        let promise_id = engine::PromiseId::new("promise_1");
        let payload_ref = BlobRef::from_bytes(b"child output");
        let mut workflow = AgentSessionWorkflow::default();
        workflow.core_state.lifecycle.status = CoreAgentStatus::Open;
        workflow.core_state.promises.promises.insert(
            promise_id.clone(),
            engine::Promise {
                promise_id: promise_id.clone(),
                source: engine::PromiseSource::Timer { fire_at_ms: 1 },
                scope: engine::PromiseScope::Session,
                ownership: engine::PromiseOwnership::Model,
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
