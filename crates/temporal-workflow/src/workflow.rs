use std::time::Duration;
use std::{collections::BTreeMap, time::UNIX_EPOCH};

use engine::{
    ApplyEvent, BlobRef, CommandCodec, CommandError, ContextEntryInput, ContextEntryKey,
    ContextEntryKind, CoreAgentAction, CoreAgentCodec, CoreAgentCommand, CoreAgentDrive,
    CoreAgentDriveError, CoreAgentEntry, CoreAgentEventKind, CoreAgentState, CoreApplyEvent,
    ENVIRONMENT_ACTIVE_CONTEXT_KEY, ENVIRONMENT_CATALOG_CONTEXT_KEY, LlmGenerationRequest,
    RunEvent, RunStatus, SKILL_CATALOG_CONTEXT_KEY, SessionId, SessionPosition, SubmissionId,
    ToolCallStatus, ToolEvent, ToolInvocationBatchRequest, ToolInvocationBatchResult,
    ToolInvocationResult, VFS_CATALOG_CONTEXT_KEY,
};
use futures::{FutureExt, pin_mut, select};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ContinueAsNewOptions, SyncWorkflowContext, WorkflowContext, WorkflowContextView, WorkflowResult,
};

use crate::{
    ActiveWaitRecord, ActiveWaitSubscription, AgentActiveRunSummary, AgentAdmission,
    AgentAdmissionFailure, AgentAdmissionFailureKind, AgentCompletedRunSummary,
    AgentQueuedRunSummary, AgentSessionArgs, AgentSessionStatus, AgentWaitDirective,
    AgentWaitHandleStatus, AgentWaitMode, AgentWaitOutcome, AgentWaitOutput, AgentWaitRunResult,
    AppendEventsRequest, CreateOrLoadSessionRequest, DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD,
    FLEET_AGENT_WAIT_DIRECTIVE_KIND, LlmGenerateActivityRequest, PendingRunTerminalNotification,
    PendingToolBatchResume, PreprocessRunInputActivityRequest, PreprocessRunInputFailure,
    PreprocessRunInputFailureKind, PreprocessRunInputOutcome, PutBlobRequest, RunSubscription,
    RunTerminalNotification, SkillCatalogRefreshActivityRequest, ToolInvokeBatchActivityRequest,
    WorkflowActivities, activity_options, default_instructions,
};

const DEFAULT_MAX_STEPS_PER_INPUT: usize = 256;

#[workflow(name = "AgentSessionWorkflow")]
pub struct AgentSessionWorkflow {
    session_id: Option<SessionId>,
    initialized: bool,
    core_state: CoreAgentState,
    head: Option<SessionPosition>,
    pending_admissions: Vec<AgentAdmission>,
    pending_tool_batch_resumes: Vec<PendingToolBatchResume>,
    pending_terminal_notifications: Vec<PendingRunTerminalNotification>,
    active_waits: BTreeMap<u64, ActiveWaitRecord>,
    run_subscriptions: BTreeMap<String, RunSubscription>,
    run_submissions: BTreeMap<u64, Option<SubmissionId>>,
    admission_failures: Vec<AgentAdmissionFailure>,
    last_error: Option<String>,
    bootstrap_failed: bool,
}

impl Default for AgentSessionWorkflow {
    fn default() -> Self {
        Self {
            session_id: None,
            initialized: false,
            core_state: CoreAgentState::new(),
            head: None,
            pending_admissions: Vec::new(),
            pending_tool_batch_resumes: Vec::new(),
            pending_terminal_notifications: Vec::new(),
            active_waits: BTreeMap::new(),
            run_subscriptions: BTreeMap::new(),
            run_submissions: BTreeMap::new(),
            admission_failures: Vec::new(),
            last_error: None,
            bootstrap_failed: false,
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
            record_bootstrap_error(ctx, &error);
            return Err(anyhow::anyhow!("{error}").into());
        }

        loop {
            wait_for_workflow_work(ctx).await;
            if let Err(error) = flush_pending_terminal_notifications(ctx).await {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            if let Err(error) = process_satisfied_active_waits(ctx).await {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            if let Err(error) = process_pending_tool_batch_resumes(ctx, &args).await {
                record_error(ctx, &error);
                return Err(anyhow::anyhow!("{error}").into());
            }
            let admissions = ctx.state_mut(|state| std::mem::take(&mut state.pending_admissions));
            if !admissions.is_empty()
                && let Err(error) = process_admissions(ctx, &args, admissions).await
            {
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

    #[signal(name = "subscribe_run")]
    pub fn subscribe_run(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        subscription: RunSubscription,
    ) {
        self.subscribe_to_run(subscription);
    }

    #[signal(name = "unsubscribe_run")]
    pub fn unsubscribe_run(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        subscription_id: String,
    ) {
        self.unsubscribe_from_run(&subscription_id);
    }

    #[signal(name = "run_terminal")]
    pub fn run_terminal(
        &mut self,
        _ctx: &mut SyncWorkflowContext<Self>,
        notification: RunTerminalNotification,
    ) {
        self.record_run_terminal(notification);
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

    pub fn subscribe_to_run(&mut self, subscription: RunSubscription) {
        if let Some(notification) =
            terminal_notification_for_state(&self.core_state, subscription.run_id)
        {
            self.pending_terminal_notifications
                .push(PendingRunTerminalNotification {
                    notification: notification
                        .with_correlation_token(subscription.correlation_token.clone()),
                    subscription,
                });
            return;
        }
        self.run_subscriptions
            .insert(subscription.subscription_id.clone(), subscription);
    }

    pub fn unsubscribe_from_run(&mut self, subscription_id: &str) {
        self.run_subscriptions.remove(subscription_id);
        self.pending_terminal_notifications
            .retain(|pending| pending.subscription.subscription_id != subscription_id);
    }

    pub fn record_run_terminal(&mut self, notification: RunTerminalNotification) {
        for wait in self.active_waits.values_mut() {
            mark_wait_terminal_arrival(wait, &notification);
        }
    }

    fn queue_terminal_notifications_for_entries(&mut self, entries: &[CoreAgentEntry]) {
        for entry in entries {
            if let Some(notification) = terminal_notification_for_event(&entry.event.kind) {
                self.queue_terminal_notifications(notification);
            }
        }
    }

    fn queue_terminal_notifications(&mut self, notification: RunTerminalNotification) {
        let subscription_ids = self
            .run_subscriptions
            .iter()
            .filter_map(|(subscription_id, subscription)| {
                (subscription.run_id == notification.run_id).then_some(subscription_id.clone())
            })
            .collect::<Vec<_>>();
        for subscription_id in subscription_ids {
            let Some(subscription) = self.run_subscriptions.remove(&subscription_id) else {
                continue;
            };
            self.pending_terminal_notifications
                .push(PendingRunTerminalNotification {
                    notification: notification
                        .clone()
                        .with_correlation_token(subscription.correlation_token.clone()),
                    subscription,
                });
        }
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
            pending_tool_batch_resumes: self.pending_tool_batch_resumes.len(),
            active_waits: self.active_waits.len(),
            run_subscriptions: self.run_subscriptions.len(),
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
            bootstrap_failed: self.bootstrap_failed,
        }
    }
}

impl RunTerminalNotification {
    fn with_correlation_token(mut self, correlation_token: String) -> Self {
        self.correlation_token = correlation_token;
        self
    }
}

fn terminal_notification_for_state(
    state: &CoreAgentState,
    run_id: engine::RunId,
) -> Option<RunTerminalNotification> {
    state
        .runs
        .completed
        .iter()
        .find(|record| record.run_id == run_id)
        .map(|record| RunTerminalNotification {
            correlation_token: String::new(),
            run_id,
            status: record.status,
            output_ref: record.output_ref.clone(),
            failure_message_ref: record
                .failure
                .as_ref()
                .and_then(|failure| failure.message_ref.clone()),
        })
}

fn terminal_notification_for_event(event: &CoreAgentEventKind) -> Option<RunTerminalNotification> {
    match event {
        CoreAgentEventKind::Run(RunEvent::Completed { run_id, output_ref }) => {
            Some(RunTerminalNotification {
                correlation_token: String::new(),
                run_id: *run_id,
                status: RunStatus::Completed,
                output_ref: output_ref.clone(),
                failure_message_ref: None,
            })
        }
        CoreAgentEventKind::Run(RunEvent::Failed { run_id, failure }) => {
            Some(RunTerminalNotification {
                correlation_token: String::new(),
                run_id: *run_id,
                status: RunStatus::Failed,
                output_ref: None,
                failure_message_ref: failure.message_ref.clone(),
            })
        }
        CoreAgentEventKind::Run(RunEvent::Cancelled { run_id }) => Some(RunTerminalNotification {
            correlation_token: String::new(),
            run_id: *run_id,
            status: RunStatus::Cancelled,
            output_ref: None,
            failure_message_ref: None,
        }),
        _ => None,
    }
}

fn wait_directive_for_event(event: &CoreAgentEventKind) -> anyhow::Result<Option<DeferredWait>> {
    let CoreAgentEventKind::Tool(ToolEvent::BatchDeferred {
        run_id,
        turn_id,
        batch_id,
        resume_directive,
    }) = event
    else {
        return Ok(None);
    };
    if resume_directive.api_kind != FLEET_AGENT_WAIT_DIRECTIVE_KIND {
        return Ok(None);
    }
    let directive: AgentWaitDirective = serde_json::from_value(resume_directive.body.clone())
        .map_err(|error| anyhow::anyhow!("invalid agent_wait resume directive: {error}"))?;
    Ok(Some(DeferredWait {
        run_id: *run_id,
        turn_id: *turn_id,
        batch_id: *batch_id,
        directive,
    }))
}

#[derive(Clone, Debug)]
struct DeferredWait {
    run_id: engine::RunId,
    turn_id: engine::TurnId,
    batch_id: engine::ToolBatchId,
    directive: AgentWaitDirective,
}

fn mark_wait_terminal_arrival(wait: &mut ActiveWaitRecord, notification: &RunTerminalNotification) {
    let Some(subscription) = wait.subscriptions.iter().find(|subscription| {
        subscription.subscription.correlation_token == notification.correlation_token
    }) else {
        return;
    };
    let target_session_id = subscription.target_session_id.as_str();
    let run_id = api_run_id(notification.run_id);
    let Some(result) = wait
        .results
        .iter_mut()
        .find(|result| result.target_session_id == target_session_id && result.run_id == run_id)
    else {
        return;
    };
    if result.status == AgentWaitHandleStatus::Terminal {
        return;
    }
    result.status = AgentWaitHandleStatus::Terminal;
    result.run = Some(AgentWaitRunResult {
        status: run_status_name(notification.status),
        output_ref: notification.output_ref.clone(),
        failure_message_ref: notification.failure_message_ref.clone(),
    });
    result.error = None;
}

fn mark_wait_handle_error(
    wait: &mut ActiveWaitRecord,
    target_session_id: &SessionId,
    run_id: engine::RunId,
    error: impl Into<String>,
) {
    let api_run_id = api_run_id(run_id);
    let Some(result) = wait.results.iter_mut().find(|result| {
        result.target_session_id == target_session_id.as_str() && result.run_id == api_run_id
    }) else {
        return;
    };
    if result.status == AgentWaitHandleStatus::Terminal {
        return;
    }
    result.status = AgentWaitHandleStatus::Error;
    result.run = None;
    result.error = Some(error.into());
}

fn run_status_name(status: RunStatus) -> String {
    match status {
        RunStatus::Active => "running",
        RunStatus::Cancelling => "cancelling",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
    .to_owned()
}

fn api_run_id(run_id: engine::RunId) -> String {
    format!("run_{}", run_id.as_u64())
}

fn wait_subscription_id(
    batch_id: engine::ToolBatchId,
    target_session_id: &SessionId,
    run_id: engine::RunId,
) -> String {
    format!(
        "fleet_wait_{}_{}_{}",
        batch_id.as_u64(),
        target_session_id.as_str(),
        run_id.as_u64()
    )
}

fn wait_correlation_token(
    batch_id: engine::ToolBatchId,
    target_session_id: &SessionId,
    run_id: engine::RunId,
) -> String {
    format!(
        "fleet_wait:{}:{}:{}",
        batch_id.as_u64(),
        target_session_id.as_str(),
        run_id.as_u64()
    )
}

fn wait_model_visible_text(output: &AgentWaitOutput) -> String {
    let terminal = output
        .results
        .iter()
        .filter(|result| result.status == AgentWaitHandleStatus::Terminal)
        .count();
    let pending = output
        .results
        .iter()
        .filter(|result| result.status == AgentWaitHandleStatus::Pending)
        .count();
    let errors = output
        .results
        .iter()
        .filter(|result| result.status == AgentWaitHandleStatus::Error)
        .count();
    format!(
        "agent_wait resolved with outcome {} (terminal: {terminal}, pending: {pending}, errors: {errors}).",
        wait_outcome_name(output.outcome)
    )
}

fn wait_outcome_name(outcome: AgentWaitOutcome) -> &'static str {
    match outcome {
        AgentWaitOutcome::Terminal => "terminal",
        AgentWaitOutcome::Timeout => "timeout",
        AgentWaitOutcome::Error => "error",
    }
}

fn active_wait_resolution(wait: &ActiveWaitRecord, now_ms: u64) -> Option<AgentWaitOutcome> {
    if wait
        .deadline_ms
        .is_some_and(|deadline_ms| deadline_ms <= now_ms)
    {
        return Some(AgentWaitOutcome::Timeout);
    }
    active_wait_nontimer_resolution(wait)
}

fn active_wait_nontimer_resolution(wait: &ActiveWaitRecord) -> Option<AgentWaitOutcome> {
    match wait.mode {
        AgentWaitMode::All => {
            if wait
                .results
                .iter()
                .any(|result| result.status == AgentWaitHandleStatus::Error)
            {
                Some(AgentWaitOutcome::Error)
            } else if wait
                .results
                .iter()
                .all(|result| result.status == AgentWaitHandleStatus::Terminal)
            {
                Some(AgentWaitOutcome::Terminal)
            } else {
                None
            }
        }
        AgentWaitMode::Any => {
            if wait
                .results
                .iter()
                .any(|result| result.status == AgentWaitHandleStatus::Terminal)
            {
                Some(AgentWaitOutcome::Terminal)
            } else if wait
                .results
                .iter()
                .all(|result| result.status == AgentWaitHandleStatus::Error)
            {
                Some(AgentWaitOutcome::Error)
            } else {
                None
            }
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
    // the activity reduces the durable log internally and returns compact
    // state. The full event log no longer crosses the activity boundary, so this
    // bootstrap path is bounded by active context size, not total log length.
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

    let is_fresh_session = loaded.replayed_event_count == 0;
    let core_state = loaded.core_state.unwrap_or_else(CoreAgentState::new);
    let run_submissions = loaded.run_submissions;
    let head = loaded.head;
    ctx.state_mut(|state| {
        state.session_id = Some(args.session_id.clone());
        state.core_state = core_state;
        state.head = head;
        state.run_submissions = run_submissions;
        state.initialized = true;
        state.last_error = None;
    });

    if is_fresh_session {
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

fn command_needs_run_input_preprocessing(command: &CoreAgentCommand) -> bool {
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

fn preprocess_failure_to_admission_failure(
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

async fn wait_for_workflow_work(ctx: &mut WorkflowContext<AgentSessionWorkflow>) {
    let now = workflow_time_ms(ctx);
    if workflow_has_immediate_work(ctx, now) {
        return;
    }

    let Some(deadline_ms) = nearest_active_wait_deadline_ms(ctx) else {
        ctx.wait_condition(|state| workflow_state_has_immediate_work(state))
            .await;
        return;
    };
    if deadline_ms <= now {
        return;
    }

    let duration = Duration::from_millis(deadline_ms - now);
    let wake = {
        let wait = ctx.wait_condition(|state| workflow_state_has_immediate_work(state));
        let timer = ctx.timer(duration).fuse();
        pin_mut!(wait, timer);
        select! {
            _ = wait => WorkflowWake::State,
            _ = timer => WorkflowWake::Timer,
        }
    };
    let _ = wake;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkflowWake {
    State,
    Timer,
}

fn workflow_has_immediate_work(ctx: &WorkflowContext<AgentSessionWorkflow>, now: u64) -> bool {
    ctx.state(|state| {
        workflow_state_has_immediate_work(state)
            || nearest_active_wait_deadline_ms_for_state(state)
                .is_some_and(|deadline| deadline <= now)
    })
}

fn workflow_state_has_immediate_work(state: &AgentSessionWorkflow) -> bool {
    !state.pending_admissions.is_empty()
        || !state.pending_tool_batch_resumes.is_empty()
        || !state.pending_terminal_notifications.is_empty()
        || state
            .active_waits
            .values()
            .any(|wait| active_wait_nontimer_resolution(wait).is_some())
}

fn nearest_active_wait_deadline_ms(ctx: &WorkflowContext<AgentSessionWorkflow>) -> Option<u64> {
    ctx.state(nearest_active_wait_deadline_ms_for_state)
}

fn nearest_active_wait_deadline_ms_for_state(state: &AgentSessionWorkflow) -> Option<u64> {
    state
        .active_waits
        .values()
        .filter_map(|wait| wait.deadline_ms)
        .min()
}

async fn flush_pending_terminal_notifications(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let pending = ctx.state_mut(|state| std::mem::take(&mut state.pending_terminal_notifications));
    for pending in pending {
        let _ = ctx
            .external_workflow(pending.subscription.subscriber_workflow_id, None)
            .signal(AgentSessionWorkflow::run_terminal, pending.notification)
            .await;
    }
    Ok(())
}

async fn install_deferred_wait(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    deferred: DeferredWait,
) -> anyhow::Result<()> {
    let now = workflow_time_ms(ctx);
    let deadline_ms = deferred
        .directive
        .timeout_ms
        .map(|timeout_ms| now.saturating_add(timeout_ms));
    let subscriptions = deferred
        .directive
        .handles
        .iter()
        .filter(|handle| {
            deferred.directive.results.iter().any(|result| {
                result.target_session_id == handle.target_session_id.as_str()
                    && result.run_id == api_run_id(handle.run_id)
                    && result.status == AgentWaitHandleStatus::Pending
            })
        })
        .map(|handle| {
            let subscription = RunSubscription {
                subscription_id: wait_subscription_id(
                    deferred.batch_id,
                    &handle.target_session_id,
                    handle.run_id,
                ),
                subscriber_workflow_id: ctx.workflow_id().to_owned(),
                correlation_token: wait_correlation_token(
                    deferred.batch_id,
                    &handle.target_session_id,
                    handle.run_id,
                ),
                run_id: handle.run_id,
            };
            ActiveWaitSubscription {
                target_session_id: handle.target_session_id.clone(),
                subscription,
            }
        })
        .collect::<Vec<_>>();
    let wait = ActiveWaitRecord {
        batch_id: deferred.batch_id,
        run_id: deferred.run_id,
        turn_id: deferred.turn_id,
        call_id: deferred.directive.call_id,
        mode: deferred.directive.mode,
        handles: deferred.directive.handles,
        results: deferred.directive.results,
        subscriptions: subscriptions.clone(),
        deadline_ms,
    };
    ctx.state_mut(|state| {
        state.active_waits.insert(wait.batch_id.as_u64(), wait);
    });

    for subscription in subscriptions {
        let signal_result = ctx
            .external_workflow(subscription.target_session_id.as_str().to_owned(), None)
            .signal(
                AgentSessionWorkflow::subscribe_run,
                subscription.subscription.clone(),
            )
            .await;
        if let Err(error) = signal_result {
            ctx.state_mut(|state| {
                if let Some(wait) = state.active_waits.get_mut(&deferred.batch_id.as_u64()) {
                    mark_wait_handle_error(
                        wait,
                        &subscription.target_session_id,
                        subscription.subscription.run_id,
                        format!("subscribe_run signal failed: {error}"),
                    );
                }
            });
        }
    }
    Ok(())
}

async fn process_satisfied_active_waits(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
) -> anyhow::Result<()> {
    let now = workflow_time_ms(ctx);
    let resolved = ctx.state_mut(|state| {
        let resolved_batch_ids = state
            .active_waits
            .iter()
            .filter_map(|(batch_id, wait)| {
                active_wait_resolution(wait, now).map(|outcome| (*batch_id, outcome))
            })
            .collect::<Vec<_>>();
        resolved_batch_ids
            .into_iter()
            .filter_map(|(batch_id, outcome)| {
                state
                    .active_waits
                    .remove(&batch_id)
                    .map(|wait| (wait, outcome))
            })
            .collect::<Vec<_>>()
    });
    for (wait, outcome) in resolved {
        unsubscribe_wait_subscriptions(ctx, &wait).await;
        let result = build_wait_tool_batch_result(ctx, wait, outcome).await?;
        ctx.state_mut(|state| {
            state
                .pending_tool_batch_resumes
                .push(PendingToolBatchResume {
                    batch_id: result.batch_id,
                    result,
                });
        });
    }
    Ok(())
}

async fn unsubscribe_wait_subscriptions(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    wait: &ActiveWaitRecord,
) {
    for subscription in &wait.subscriptions {
        if wait.results.iter().any(|result| {
            result.target_session_id == subscription.target_session_id.as_str()
                && result.run_id == api_run_id(subscription.subscription.run_id)
                && result.status == AgentWaitHandleStatus::Terminal
        }) {
            continue;
        }
        let _ = ctx
            .external_workflow(subscription.target_session_id.as_str().to_owned(), None)
            .signal(
                AgentSessionWorkflow::unsubscribe_run,
                subscription.subscription.subscription_id.clone(),
            )
            .await;
    }
}

async fn build_wait_tool_batch_result(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    wait: ActiveWaitRecord,
    outcome: AgentWaitOutcome,
) -> anyhow::Result<ToolInvocationBatchResult> {
    let output = AgentWaitOutput {
        outcome,
        results: wait.results,
    };
    let output_ref = ctx
        .start_activity(
            WorkflowActivities::put_blob,
            PutBlobRequest {
                bytes: serde_json::to_vec(&output)?,
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let visible_ref = ctx
        .start_activity(
            WorkflowActivities::put_blob,
            PutBlobRequest {
                bytes: wait_model_visible_text(&output).into_bytes(),
            },
            activity_options(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(ToolInvocationBatchResult {
        run_id: wait.run_id,
        turn_id: wait.turn_id,
        batch_id: wait.batch_id,
        results: vec![ToolInvocationResult {
            call_id: wait.call_id,
            status: ToolCallStatus::Succeeded,
            output_ref: Some(output_ref),
            model_visible_output_ref: Some(visible_ref),
            error_ref: None,
            effects: Vec::new(),
        }],
    })
}

async fn process_pending_tool_batch_resumes(
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
        match admit_and_append_command(ctx, &mut drive, command).await? {
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
    let deferred_waits = entries
        .iter()
        .filter_map(|entry| wait_directive_for_event(&entry.event.kind).transpose())
        .collect::<anyhow::Result<Vec<_>>>()?;
    ctx.state_mut(|state| -> anyhow::Result<()> {
        apply_entries(
            &CoreApplyEvent,
            &mut state.core_state,
            &entries,
            &mut state.run_submissions,
        )?;
        state.queue_terminal_notifications_for_entries(&entries);
        state.head = appended.head;
        state.last_error = None;
        Ok(())
    })?;
    for wait in deferred_waits {
        install_deferred_wait(ctx, wait).await?;
    }
    Ok(entries)
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

async fn call_context_compact(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: engine::ContextCompactionRequest,
) -> anyhow::Result<engine::ContextCompactionResult> {
    ctx.start_activity(
        WorkflowActivities::context_compact,
        crate::ContextCompactActivityRequest { request },
        activity_options(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))
}

async fn call_tool_invoke_batch(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: ToolInvocationBatchRequest,
) -> anyhow::Result<engine::ToolBatchOutcome> {
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

/// Record a failure that occurred during session bootstrap (rehydration). This
/// is surfaced distinctly from ordinary run errors so the gateway/bridge can
/// report a typed `session_bootstrap_failed` recovery problem instead of a
/// generic message-answer failure.
fn record_bootstrap_error(ctx: &WorkflowContext<AgentSessionWorkflow>, error: &anyhow::Error) {
    let message = error.to_string();
    ctx.state_mut(|state| {
        state.last_error = Some(message);
        state.bootstrap_failed = true;
    });
}

fn can_continue_as_new_at_idle(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> bool {
    ctx.state(workflow_state_allows_continue_as_new)
        && should_continue_as_new(
            ctx.continue_as_new_suggested(),
            ctx.history_length(),
            args.continue_as_new_history_threshold,
        )
}

fn workflow_state_allows_continue_as_new(state: &AgentSessionWorkflow) -> bool {
    state.pending_admissions.is_empty()
        && state.pending_tool_batch_resumes.is_empty()
        && state.pending_terminal_notifications.is_empty()
        && state.active_waits.is_empty()
        && state.run_subscriptions.is_empty()
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
    use engine::{
        ContextEntryInput, ContextEntryKind, ContextMessageRole, DynamicCommand, RunId, RunRecord,
        RunStatus, ToolBatchId, ToolInvocationBatchResult, TurnId,
    };

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
    fn request_run_with_audio_input_needs_preprocessing() {
        let command = CoreAgentCommand::RequestRun {
            submission_id: Some(SubmissionId::new("submit_audio")),
            input: vec![ContextEntryInput {
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                content_ref: engine::BlobRef::from_bytes(b"audio"),
                media_type: Some("audio/ogg".to_owned()),
                preview: Some("[audio]".to_owned()),
                provider_kind: None,
                provider_item_id: None,
                token_estimate: None,
            }],
            run_config: crate::default_run_config(),
        };

        assert!(command_needs_run_input_preprocessing(&command));
    }

    #[test]
    fn preprocess_failures_preserve_submission_id_for_admission_failure() {
        let failure = preprocess_failure_to_admission_failure(
            Some(SubmissionId::new("submit_audio")),
            PreprocessRunInputFailure {
                kind: PreprocessRunInputFailureKind::TranscriptionFailure,
                message: "missing OpenAI key".to_owned(),
            },
        );

        assert_eq!(
            failure.submission_id.as_ref(),
            Some(&SubmissionId::new("submit_audio"))
        );
        assert_eq!(
            failure.kind,
            AgentAdmissionFailureKind::TranscriptionFailure
        );
    }

    #[test]
    fn subscribe_run_against_completed_run_queues_notification_without_storing_subscription() {
        let mut workflow = AgentSessionWorkflow::default();
        let output_ref = engine::BlobRef::from_bytes(b"done");
        workflow.core_state.runs.completed.push(RunRecord {
            run_id: RunId::new(1),
            status: RunStatus::Completed,
            submission_id: None,
            submission_digest: None,
            output_ref: Some(output_ref.clone()),
            failure: None,
        });

        let subscription = run_subscription("sub_1", "token_1", 1);
        workflow.subscribe_to_run(subscription.clone());

        assert!(workflow.run_subscriptions.is_empty());
        assert_eq!(workflow.pending_terminal_notifications.len(), 1);
        let pending = &workflow.pending_terminal_notifications[0];
        assert_eq!(pending.subscription, subscription);
        assert_eq!(pending.notification.correlation_token, "token_1");
        assert_eq!(pending.notification.run_id, RunId::new(1));
        assert_eq!(pending.notification.status, RunStatus::Completed);
        assert_eq!(pending.notification.output_ref.as_ref(), Some(&output_ref));
    }

    #[test]
    fn terminal_event_fanout_removes_matching_subscriptions_once() {
        let mut workflow = AgentSessionWorkflow::default();
        workflow.subscribe_to_run(run_subscription("sub_1", "token_1", 1));
        workflow.subscribe_to_run(run_subscription("sub_2", "token_2", 1));
        workflow.subscribe_to_run(run_subscription("sub_3", "token_3", 2));

        workflow.queue_terminal_notifications(terminal_notification("", 1, RunStatus::Completed));

        assert_eq!(workflow.run_subscriptions.len(), 1);
        assert!(workflow.run_subscriptions.contains_key("sub_3"));
        assert_eq!(workflow.pending_terminal_notifications.len(), 2);
        let tokens = workflow
            .pending_terminal_notifications
            .iter()
            .map(|pending| pending.notification.correlation_token.as_str())
            .collect::<Vec<_>>();
        assert_eq!(tokens, vec!["token_1", "token_2"]);

        workflow.queue_terminal_notifications(terminal_notification("", 1, RunStatus::Completed));

        assert_eq!(workflow.pending_terminal_notifications.len(), 2);
        assert_eq!(workflow.run_subscriptions.len(), 1);
    }

    #[test]
    fn unsubscribe_run_removes_stored_and_pending_notifications() {
        let mut workflow = AgentSessionWorkflow::default();
        let stored = run_subscription("sub_stored", "token_stored", 1);
        let pending = run_subscription("sub_pending", "token_pending", 1);
        workflow
            .run_subscriptions
            .insert(stored.subscription_id.clone(), stored);
        workflow
            .pending_terminal_notifications
            .push(PendingRunTerminalNotification {
                subscription: pending,
                notification: terminal_notification("token_pending", 1, RunStatus::Completed),
            });

        workflow.unsubscribe_from_run("sub_stored");

        assert!(workflow.run_subscriptions.is_empty());
        assert_eq!(workflow.pending_terminal_notifications.len(), 1);

        workflow.unsubscribe_from_run("sub_pending");

        assert!(workflow.pending_terminal_notifications.is_empty());
    }

    #[test]
    fn run_terminal_signal_records_active_wait_arrival_idempotently() {
        let mut workflow = AgentSessionWorkflow::default();
        workflow
            .active_waits
            .insert(7, active_wait_record(7, "token_1"));
        let notification = terminal_notification("token_1", 1, RunStatus::Completed);

        workflow.record_run_terminal(notification.clone());
        workflow.record_run_terminal(notification);
        workflow.record_run_terminal(terminal_notification("other", 1, RunStatus::Completed));

        let wait = workflow.active_waits.get(&7).expect("active wait");
        assert_eq!(wait.results.len(), 1);
        assert_eq!(wait.results[0].status, AgentWaitHandleStatus::Terminal);
        assert_eq!(wait.results[0].target_session_id, "target_session");
        assert_eq!(wait.results[0].run_id, "run_1");
        assert_eq!(
            wait.results[0].run.as_ref().map(|run| run.status.as_str()),
            Some("completed")
        );
    }

    #[test]
    fn all_mode_active_wait_resolves_after_all_child_notifications_arrive() {
        let mut wait = active_wait_record(7, "token_1");
        let second_target_session_id = SessionId::new("target_session_two");
        let second_run_id = RunId::new(2);
        wait.handles.push(crate::AgentWaitHandle {
            target_session_id: second_target_session_id.clone(),
            run_id: second_run_id,
        });
        wait.results.push(crate::AgentWaitHandleResult {
            target_session_id: second_target_session_id.as_str().to_owned(),
            run_id: api_run_id(second_run_id),
            status: AgentWaitHandleStatus::Pending,
            run: None,
            error: None,
        });
        wait.subscriptions.push(ActiveWaitSubscription {
            target_session_id: second_target_session_id,
            subscription: RunSubscription {
                subscription_id: "sub_wait_two".to_owned(),
                subscriber_workflow_id: "subscriber_session".to_owned(),
                correlation_token: "token_2".to_owned(),
                run_id: second_run_id,
            },
        });

        let mut workflow = AgentSessionWorkflow::default();
        workflow.active_waits.insert(7, wait);
        assert_eq!(
            active_wait_nontimer_resolution(workflow.active_waits.get(&7).expect("active wait")),
            None
        );

        workflow.record_run_terminal(terminal_notification("token_1", 1, RunStatus::Completed));
        let wait = workflow.active_waits.get(&7).expect("active wait");
        assert_eq!(active_wait_nontimer_resolution(wait), None);
        assert_eq!(
            wait.results
                .iter()
                .filter(|result| result.status == AgentWaitHandleStatus::Terminal)
                .count(),
            1
        );

        workflow.record_run_terminal(terminal_notification("token_2", 2, RunStatus::Completed));
        let wait = workflow.active_waits.get(&7).expect("active wait");
        assert_eq!(
            active_wait_nontimer_resolution(wait),
            Some(AgentWaitOutcome::Terminal)
        );
        assert!(
            wait.results
                .iter()
                .all(|result| result.status == AgentWaitHandleStatus::Terminal)
        );
    }

    #[test]
    fn continue_as_new_is_blocked_by_waits_subscriptions_and_pending_work() {
        let mut workflow = AgentSessionWorkflow::default();
        assert!(workflow_state_allows_continue_as_new(&workflow));

        workflow.queue_admission(admission(encoded_request_run("submit_1")));
        assert!(!workflow_state_allows_continue_as_new(&workflow));
        workflow.pending_admissions.clear();

        workflow.pending_tool_batch_resumes.push(pending_resume(1));
        assert!(!workflow_state_allows_continue_as_new(&workflow));
        workflow.pending_tool_batch_resumes.clear();

        workflow
            .pending_terminal_notifications
            .push(PendingRunTerminalNotification {
                subscription: run_subscription("sub_pending", "token_pending", 1),
                notification: terminal_notification("token_pending", 1, RunStatus::Completed),
            });
        assert!(!workflow_state_allows_continue_as_new(&workflow));
        workflow.pending_terminal_notifications.clear();

        workflow
            .active_waits
            .insert(7, active_wait_record(7, "token_1"));
        assert!(!workflow_state_allows_continue_as_new(&workflow));
        workflow.active_waits.clear();

        workflow.subscribe_to_run(run_subscription("sub_1", "token_1", 1));
        assert!(!workflow_state_allows_continue_as_new(&workflow));
        workflow.run_subscriptions.clear();

        assert!(workflow_state_allows_continue_as_new(&workflow));
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

    fn run_subscription(
        subscription_id: &str,
        correlation_token: &str,
        run_id: u64,
    ) -> RunSubscription {
        RunSubscription {
            subscription_id: subscription_id.to_owned(),
            subscriber_workflow_id: "subscriber_session".to_owned(),
            correlation_token: correlation_token.to_owned(),
            run_id: RunId::new(run_id),
        }
    }

    fn terminal_notification(
        correlation_token: &str,
        run_id: u64,
        status: RunStatus,
    ) -> RunTerminalNotification {
        RunTerminalNotification {
            correlation_token: correlation_token.to_owned(),
            run_id: RunId::new(run_id),
            status,
            output_ref: None,
            failure_message_ref: None,
        }
    }

    fn active_wait_record(batch_id: u64, correlation_token: &str) -> ActiveWaitRecord {
        let target_session_id = SessionId::new("target_session");
        let run_id = RunId::new(1);
        ActiveWaitRecord {
            batch_id: ToolBatchId::new(batch_id),
            run_id: RunId::new(10),
            turn_id: TurnId::new(20),
            call_id: engine::ToolCallId::new("call_wait"),
            mode: AgentWaitMode::All,
            handles: vec![crate::AgentWaitHandle {
                target_session_id: target_session_id.clone(),
                run_id,
            }],
            results: vec![crate::AgentWaitHandleResult {
                target_session_id: target_session_id.as_str().to_owned(),
                run_id: api_run_id(run_id),
                status: AgentWaitHandleStatus::Pending,
                run: None,
                error: None,
            }],
            subscriptions: vec![ActiveWaitSubscription {
                target_session_id,
                subscription: RunSubscription {
                    subscription_id: "sub_wait".to_owned(),
                    subscriber_workflow_id: "subscriber_session".to_owned(),
                    correlation_token: correlation_token.to_owned(),
                    run_id,
                },
            }],
            deadline_ms: None,
        }
    }

    fn pending_resume(batch_id: u64) -> PendingToolBatchResume {
        PendingToolBatchResume {
            batch_id: ToolBatchId::new(batch_id),
            result: ToolInvocationBatchResult {
                run_id: RunId::new(1),
                turn_id: TurnId::new(1),
                batch_id: ToolBatchId::new(batch_id),
                results: Vec::new(),
            },
        }
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
