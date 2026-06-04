use std::{collections::BTreeMap, time::UNIX_EPOCH};

use engine::{
    ApplyEvent, BlobRef, CommandCodec, CommandError, ContextEntryInput, ContextEntryKey,
    ContextEntryKind, CoreAgentAction, CoreAgentCodec, CoreAgentCommand, CoreAgentDrive,
    CoreAgentDriveError, CoreAgentEntry, CoreAgentEventKind, CoreAgentState, CoreApplyEvent,
    LlmGenerationRequest, RunEvent, SessionId, SessionPosition, SubmissionId,
    ToolInvocationBatchRequest, ToolProfileId,
};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ContinueAsNewOptions, SyncWorkflowContext, WorkflowContext, WorkflowContextView, WorkflowResult,
};

use crate::{
    AgentActiveRunSummary, AgentAdmission, AgentAdmissionFailure, AgentAdmissionFailureKind,
    AgentCompletedRunSummary, AgentQueuedRunSummary, AgentSessionArgs, AgentSessionStatus,
    AppendEventsRequest, CreateOrLoadSessionRequest, DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD,
    FAKE_TOOL_PROFILE_ID, LlmGenerateActivityRequest, PutBlobRequest,
    SkillActivationRefreshActivityRequest, SkillCatalogRefreshActivityRequest,
    ToolInvokeBatchActivityRequest, WorkflowActivities, activity_options, default_instructions,
    fake_tool_input_schema, fake_tool_registry,
};

const DEFAULT_MAX_STEPS_PER_INPUT: usize = 256;

#[workflow(name = "AgentSessionWorkflow")]
pub struct AgentSessionWorkflow {
    session_id: Option<SessionId>,
    initialized: bool,
    core_state: CoreAgentState,
    head: Option<SessionPosition>,
    pending_admissions: Vec<AgentAdmission>,
    run_submissions: BTreeMap<u64, Option<SubmissionId>>,
    admission_failures: Vec<AgentAdmissionFailure>,
    last_error: Option<String>,
}

impl Default for AgentSessionWorkflow {
    fn default() -> Self {
        Self {
            session_id: None,
            initialized: false,
            core_state: CoreAgentState::new(),
            head: None,
            pending_admissions: Vec::new(),
            run_submissions: BTreeMap::new(),
            admission_failures: Vec::new(),
            last_error: None,
        }
    }
}

#[workflow_methods]
impl AgentSessionWorkflow {
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        args: AgentSessionArgs,
    ) -> WorkflowResult<()> {
        if let Err(error) = initialize(ctx, args.clone()).await {
            record_error(ctx, &error);
            return Err(anyhow::anyhow!("{error}").into());
        }

        loop {
            ctx.wait_condition(|state| !state.pending_admissions.is_empty())
                .await;
            let admissions = ctx.state_mut(|state| std::mem::take(&mut state.pending_admissions));
            if let Err(error) = process_admissions(ctx, &args, admissions).await {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            if can_continue_as_new_at_idle(ctx, &args) {
                ctx.continue_as_new(&args, ContinueAsNewOptions::default())?;
            }
        }
    }

    #[signal(name = "submit_admission")]
    pub fn submit_admission(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        admission: AgentAdmission,
    ) {
        self.queue_admission(admission);
    }

    #[query(name = "status")]
    pub fn status(&self, _ctx: &WorkflowContextView) -> AgentSessionStatus {
        self.status_snapshot()
    }
}

impl AgentSessionWorkflow {
    pub fn queue_admission(&mut self, admission: AgentAdmission) {
        self.pending_admissions.push(admission);
    }

    pub fn status_snapshot(&self) -> AgentSessionStatus {
        AgentSessionStatus {
            session_id: self
                .session_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            initialized: self.initialized,
            pending_admissions: self.pending_admissions.len(),
            active_run: self
                .core_state
                .runs
                .active
                .as_ref()
                .map(|run| AgentActiveRunSummary {
                    run_id: run.run_id.as_u64(),
                    status: run.status,
                    submission_id: run.submission_id.clone(),
                    output_ref: run.output_ref.clone(),
                    active_turn_id: run.active_turn_id.map(|id| id.as_u64()),
                    active_tool_batch_id: run.active_tool_batch_id.map(|id| id.as_u64()),
                }),
            queued_runs: self
                .core_state
                .runs
                .queued
                .iter()
                .map(|run| AgentQueuedRunSummary {
                    submission_id: run.submission_id.clone(),
                    input: run.input.clone(),
                })
                .collect(),
            completed_runs: self
                .core_state
                .runs
                .completed
                .iter()
                .map(|run| AgentCompletedRunSummary {
                    run_id: run.run_id.as_u64(),
                    status: run.status,
                    submission_id: self
                        .run_submissions
                        .get(&run.run_id.as_u64())
                        .cloned()
                        .flatten(),
                    output_ref: run.output_ref.clone(),
                    failure_message_ref: run
                        .failure
                        .as_ref()
                        .and_then(|failure| failure.message_ref.clone()),
                })
                .collect(),
            admission_failures: self.admission_failures.clone(),
            last_error: self.last_error.clone(),
        }
    }
}

async fn initialize(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: AgentSessionArgs,
) -> anyhow::Result<()> {
    if ctx.workflow_id() != args.session_id.as_str() {
        anyhow::bail!(
            "agent workflow id must equal session id: workflow_id={} session_id={}",
            ctx.workflow_id(),
            args.session_id
        );
    }
    if ctx.state(|state| state.initialized) {
        return Ok(());
    }
    let observed_at_ms = workflow_time_ms(ctx);
    let loaded = ctx
        .start_activity(
            WorkflowActivities::create_or_load_session,
            CreateOrLoadSessionRequest {
                session_id: args.session_id.clone(),
                observed_at_ms,
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    let codec = CoreAgentCodec;
    let apply = CoreApplyEvent;
    let mut core_state = CoreAgentState::new();
    let mut run_submissions = BTreeMap::new();
    let entries = loaded
        .entries
        .iter()
        .map(|entry| codec.decode_entry(entry))
        .collect::<Result<Vec<_>, _>>()?;
    apply_entries(&apply, &mut core_state, &entries, &mut run_submissions)?;
    let head = loaded.record.head.clone();
    ctx.state_mut(|state| {
        state.session_id = Some(args.session_id.clone());
        state.core_state = core_state;
        state.head = head;
        state.run_submissions = run_submissions;
        state.initialized = true;
        state.last_error = None;
    });

    if entries.is_empty() {
        open_new_session(ctx, args).await?;
    }
    Ok(())
}

async fn open_new_session(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: AgentSessionArgs,
) -> anyhow::Result<()> {
    let instructions_ref = match args.instructions_ref.clone() {
        Some(blob_ref) => Some(blob_ref),
        None => {
            let blob_ref = ctx
                .start_activity(
                    WorkflowActivities::put_blob,
                    PutBlobRequest {
                        bytes: default_instructions().as_bytes().to_vec(),
                    },
                    activity_options(),
                )
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            Some(blob_ref)
        }
    };
    let session_config = args.session_config;
    let schema_ref = ctx
        .start_activity(
            WorkflowActivities::put_blob,
            PutBlobRequest {
                bytes: fake_tool_input_schema(),
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    let mut drive = drive_from_state(ctx)?;
    append_command(
        ctx,
        &mut drive,
        CoreAgentCommand::OpenSession {
            config: session_config,
        },
    )
    .await?;
    if let Some(instructions_ref) = instructions_ref {
        append_command(
            ctx,
            &mut drive,
            CoreAgentCommand::UpsertContext {
                key: ContextEntryKey::new("instructions.000.default"),
                entry: instruction_context_input(instructions_ref),
            },
        )
        .await?;
    }
    append_command(
        ctx,
        &mut drive,
        CoreAgentCommand::SetToolRegistry {
            registry: fake_tool_registry(schema_ref),
        },
    )
    .await?;
    append_command(
        ctx,
        &mut drive,
        CoreAgentCommand::SelectToolProfile {
            profile_id: ToolProfileId::new(FAKE_TOOL_PROFILE_ID),
        },
    )
    .await?;
    Ok(())
}

fn instruction_context_input(content_ref: BlobRef) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Instructions,
        content_ref,
        media_type: Some("text/plain".to_owned()),
        preview: None,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }
}

async fn process_admissions(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
    admissions: Vec<AgentAdmission>,
) -> anyhow::Result<()> {
    let mut drive = drive_from_state(ctx)?;
    for admission in admissions {
        let command = match CoreAgentCodec.decode_command(&admission.command) {
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
                active_catalog: drive.state().skills.catalog.clone(),
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    let Some(command) = result.command else {
        return Ok(());
    };
    match admit_and_append_command(ctx, drive, command).await? {
        CommandAdmissionResult::Accepted => Ok(()),
        CommandAdmissionResult::Rejected(failure) => {
            anyhow::bail!(
                "skill catalog refresh command rejected: {}",
                failure.message
            )
        }
    }
}

enum CommandAdmissionResult {
    Accepted,
    Rejected(AgentAdmissionFailure),
}

async fn append_command(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    command: CoreAgentCommand,
) -> anyhow::Result<()> {
    match admit_and_append_command(ctx, drive, command).await? {
        CommandAdmissionResult::Accepted => Ok(()),
        CommandAdmissionResult::Rejected(failure) => {
            anyhow::bail!("workflow setup command rejected: {}", failure.message)
        }
    }
}

async fn admit_and_append_command(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    command: CoreAgentCommand,
) -> anyhow::Result<CommandAdmissionResult> {
    let submission_id = command_submission_id(&command);
    let action = match drive.admit_command(command, workflow_time_ms(ctx)) {
        Ok(action) => action,
        Err(CoreAgentDriveError::Command(CommandError::Rejected(rejection))) => {
            return Ok(CommandAdmissionResult::Rejected(AgentAdmissionFailure {
                submission_id,
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

fn command_submission_id(command: &CoreAgentCommand) -> Option<SubmissionId> {
    match command {
        CoreAgentCommand::RequestRun { submission_id, .. } => submission_id.clone(),
        _ => None,
    }
}

async fn drive_until_idle(
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
                let pending_skill_activation_command =
                    if active_tool_batch_has_results(drive.state()) {
                        skill_activation_command_for_tool_results(ctx, drive).await?
                    } else {
                        None
                    };
                append_events(ctx, drive, expected_head, events).await?;
                if let Some(command) = pending_skill_activation_command {
                    append_skill_activation_command(ctx, drive, command).await?;
                }
                action = drive.next_action(workflow_time_ms(ctx), max_steps)?;
            }
            CoreAgentAction::GenerateLlm { request } => {
                let result = call_llm_generate(ctx, request).await?;
                action = drive.resume_generation(result, workflow_time_ms(ctx))?;
            }
            CoreAgentAction::InvokeTools { request } => {
                let result = call_tool_invoke_batch(ctx, request).await?;
                action = drive.resume_tool_batch(result, workflow_time_ms(ctx))?;
            }
            CoreAgentAction::Idle | CoreAgentAction::Closed => return Ok(()),
            CoreAgentAction::StepLimitReached => {
                // Deferred for G4: step limits can happen after partial run progress.
                // Keep treating them as workflow failures until resume semantics are explicit.
                anyhow::bail!("Agent drive step limit reached: max_steps={max_steps}");
            }
        }
    }
}

async fn append_events(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    expected_head: Option<SessionPosition>,
    events: Vec<engine::storage::DynamicUncommittedSessionEvent>,
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
        apply_entries(
            &CoreApplyEvent,
            &mut state.core_state,
            &entries,
            &mut state.run_submissions,
        )?;
        state.head = appended.head;
        state.last_error = None;
        Ok(())
    })?;
    Ok(entries)
}

async fn skill_activation_command_for_tool_results(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
) -> anyhow::Result<Option<CoreAgentCommand>> {
    let state = drive.state().clone();
    let result = ctx
        .start_activity(
            WorkflowActivities::skill_activation_refresh,
            SkillActivationRefreshActivityRequest { state },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(result.command)
}

async fn append_skill_activation_command(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    command: CoreAgentCommand,
) -> anyhow::Result<()> {
    let action = drive.admit_command(command, workflow_time_ms(ctx))?;
    match action {
        CoreAgentAction::AppendEvents {
            expected_head,
            events,
        } => {
            append_events(ctx, drive, expected_head, events).await?;
            Ok(())
        }
        CoreAgentAction::Idle | CoreAgentAction::Closed => Ok(()),
        other => anyhow::bail!("skill activation refresh emitted unexpected action: {other:?}"),
    }
}

fn active_tool_batch_has_results(state: &CoreAgentState) -> bool {
    let Some(active_run) = state.runs.active.as_ref() else {
        return false;
    };
    let Some(batch_id) = active_run.active_tool_batch_id else {
        return false;
    };
    active_run
        .tool_batches
        .get(&batch_id)
        .is_some_and(|batch| batch.calls.iter().any(|call| call.result.is_some()))
}

fn drive_from_state(ctx: &WorkflowContext<AgentSessionWorkflow>) -> anyhow::Result<CoreAgentDrive> {
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

async fn call_llm_generate(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: LlmGenerationRequest,
) -> anyhow::Result<engine::LlmGenerationResult> {
    ctx.start_activity(
        WorkflowActivities::llm_generate,
        LlmGenerateActivityRequest { request },
        activity_options(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))
}

async fn call_tool_invoke_batch(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: ToolInvocationBatchRequest,
) -> anyhow::Result<engine::ToolInvocationBatchResult> {
    ctx.start_activity(
        WorkflowActivities::tool_invoke_batch,
        ToolInvokeBatchActivityRequest { request },
        activity_options(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))
}

fn apply_entries(
    apply: &CoreApplyEvent,
    state: &mut CoreAgentState,
    entries: &[CoreAgentEntry],
    run_submissions: &mut BTreeMap<u64, Option<SubmissionId>>,
) -> anyhow::Result<()> {
    for entry in entries {
        if let CoreAgentEventKind::Run(RunEvent::Accepted {
            run_id,
            submission_id,
            ..
        }) = &entry.event.kind
        {
            run_submissions.insert(run_id.as_u64(), submission_id.clone());
        }
        apply.apply(state, entry)?;
    }
    Ok(())
}

fn workflow_time_ms(ctx: &WorkflowContext<AgentSessionWorkflow>) -> u64 {
    ctx.workflow_time()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn record_admission_failure(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    failure: AgentAdmissionFailure,
) {
    ctx.state_mut(|state| {
        state.admission_failures.push(failure);
        state.last_error = None;
    });
}

fn record_error(ctx: &WorkflowContext<AgentSessionWorkflow>, error: &anyhow::Error) {
    let message = error.to_string();
    ctx.state_mut(|state| {
        state.last_error = Some(message);
    });
}

fn can_continue_as_new_at_idle(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> bool {
    ctx.state(|state| state.pending_admissions.is_empty())
        && should_continue_as_new(
            ctx.continue_as_new_suggested(),
            ctx.history_length(),
            args.continue_as_new_history_threshold,
        )
}

fn should_continue_as_new(
    suggested: bool,
    history_length: u32,
    history_threshold: Option<u32>,
) -> bool {
    suggested
        || history_length >= history_threshold.unwrap_or(DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{ContextEntryInput, ContextEntryKind, ContextMessageRole, DynamicCommand};

    #[test]
    fn pending_admissions_are_fifo() {
        let mut workflow = AgentSessionWorkflow::default();
        workflow.queue_admission(admission(encoded_request_run("submit_1")));
        workflow.queue_admission(admission(encoded_request_run("submit_2")));

        let pending = std::mem::take(&mut workflow.pending_admissions);
        assert_eq!(
            CoreAgentCodec
                .decode_command(&pending[0].command)
                .expect("decode first command")
                .submission_id_for_test(),
            Some(SubmissionId::new("submit_1"))
        );
        assert_eq!(
            CoreAgentCodec
                .decode_command(&pending[1].command)
                .expect("decode second command")
                .submission_id_for_test(),
            Some(SubmissionId::new("submit_2"))
        );
    }

    #[test]
    fn admission_failure_status_does_not_poison_later_admission() {
        let mut workflow = AgentSessionWorkflow::default();
        workflow.admission_failures.push(AgentAdmissionFailure {
            submission_id: Some(SubmissionId::new("submit_rejected")),
            kind: AgentAdmissionFailureKind::RejectedCommand,
            message: "session must be open".to_owned(),
        });
        workflow.queue_admission(admission(encoded_request_run("submit_later")));

        let status = workflow.status_snapshot();

        assert_eq!(status.pending_admissions, 1);
        assert_eq!(status.admission_failures.len(), 1);
        assert_eq!(
            status.admission_failures[0].submission_id.as_ref(),
            Some(&SubmissionId::new("submit_rejected"))
        );
        assert_eq!(
            status.admission_failures[0].kind,
            AgentAdmissionFailureKind::RejectedCommand
        );
        assert_eq!(status.last_error, None);
    }

    #[test]
    fn request_run_submission_id_is_available_for_failure_correlation() {
        let submission_id = SubmissionId::new("submit_test");
        let command = CoreAgentCommand::RequestRun {
            submission_id: Some(submission_id.clone()),
            input: user_input(engine::BlobRef::from_bytes(b"hello")),
            run_config: crate::default_run_config(),
        };

        assert_eq!(command_submission_id(&command), Some(submission_id));
        assert_eq!(command_submission_id(&CoreAgentCommand::CloseSession), None);
    }

    #[test]
    fn continue_as_new_policy_uses_server_suggestion() {
        assert!(should_continue_as_new(true, 1, Some(10)));
    }

    #[test]
    fn continue_as_new_policy_uses_history_threshold() {
        assert!(should_continue_as_new(false, 10, Some(10)));
        assert!(!should_continue_as_new(false, 9, Some(10)));
    }

    #[test]
    fn continue_as_new_policy_uses_default_threshold() {
        assert!(should_continue_as_new(
            false,
            DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD,
            None
        ));
        assert!(!should_continue_as_new(
            false,
            DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD - 1,
            None
        ));
    }

    fn encoded_request_run(submission_id: &str) -> DynamicCommand {
        CoreAgentCodec
            .encode_command(&CoreAgentCommand::RequestRun {
                submission_id: Some(SubmissionId::new(submission_id)),
                input: user_input(engine::BlobRef::from_bytes(submission_id.as_bytes())),
                run_config: crate::default_run_config(),
            })
            .expect("encode request run")
    }

    fn user_input(content_ref: engine::BlobRef) -> Vec<ContextEntryInput> {
        vec![ContextEntryInput {
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            content_ref,
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }]
    }

    fn admission(command: DynamicCommand) -> AgentAdmission {
        AgentAdmission { command }
    }

    trait CommandSubmissionIdForTest {
        fn submission_id_for_test(&self) -> Option<SubmissionId>;
    }

    impl CommandSubmissionIdForTest for CoreAgentCommand {
        fn submission_id_for_test(&self) -> Option<SubmissionId> {
            command_submission_id(self)
        }
    }
}
