//! Session preparation is serialized with admissions. Only run controls may
//! pass an outstanding observation; derived tools publish at a safe boundary.
use super::preparation_candidate::PreparationCandidate;
use super::*;
use crate::{
    SessionOperation, SessionOperationOutcome, SessionOperationRequest, SessionToolsetPreparation,
    SessionToolsetSource,
};
use api::{AgentApiError, ProfileApplySummary};
use temporalio_sdk::CancellableFuture;

#[derive(Clone, Debug)]
pub(super) enum SessionAdmission {
    Core(AgentAdmission),
    Operation(SessionOperationRequest),
    PreparedRun {
        admission: AgentAdmission,
        result: Result<SessionToolsetPreparation, AgentApiError>,
    },
}

impl SessionAdmission {
    pub(super) fn core(&self) -> Option<&AgentAdmission> {
        match self {
            Self::Core(admission) | Self::PreparedRun { admission, .. } => Some(admission),
            Self::Operation(_) => None,
        }
    }
    pub(super) fn admissible_during_turn(&self) -> bool {
        match self {
            Self::Core(admission) => admissions::admissible_during_turn(&admission.command),
            // Configuration/profile requests are rejected against active work;
            // explicit reads return the already published catalog.
            Self::Operation(_) | Self::PreparedRun { .. } => true,
        }
    }
}

pub(super) fn activity_options() -> temporalio_sdk::ActivityOptions {
    temporalio_sdk::ActivityOptions::with_close_timeouts(
        temporalio_sdk::ActivityCloseTimeouts::Both {
            start_to_close: Duration::from_secs(30),
            schedule_to_close: Duration::from_secs(90),
        },
    )
    .retry_policy(
        temporalio_common::protos::temporal::api::common::v1::RetryPolicy {
            maximum_attempts: 3,
            ..Default::default()
        },
    )
    .build()
}

fn control_admission(admission: &SessionAdmission) -> bool {
    admission.core().is_some_and(|admission| {
        admissions::admissible_during_turn(&admission.command)
            && !matches!(admission.command, CoreAgentCommand::RequestRun(_))
    })
}

/// Preparation must not make a slow registry/source read a cancellation lock.
/// Other submissions retain their order until the observation is processed.
pub(super) async fn await_activity<T, F>(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    activity: F,
) -> Result<T, AgentApiError>
where
    F: CancellableFuture<T>,
{
    pin_mut!(activity);
    loop {
        {
            let wait =
                ctx.wait_condition(|state| state.pending_admissions.iter().any(control_admission));
            pin_mut!(wait);
            futures::select_biased! {
                _ = wait => {},
                result = activity => return Ok(result),
            }
        }
        let controls = ctx.state_mut(|state| {
            let (now, later) = std::mem::take(&mut state.pending_admissions)
                .into_iter()
                .partition(control_admission);
            state.pending_admissions = later;
            now
        });
        for control in controls {
            let SessionAdmission::Core(admission) = control else {
                unreachable!()
            };
            match admit_and_append_command(
                ctx,
                drive,
                admission.command,
                admission.correlation_token,
            )
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?
            {
                CommandAdmissionResult::Accepted => {}
                CommandAdmissionResult::Rejected(failure) => record_admission_failure(ctx, failure),
            }
        }
        if drive.state().lifecycle.status != CoreAgentStatus::Open {
            activity.as_ref().get_ref().cancel();
            let _ = activity.await;
            return Err(AgentApiError::rejected("session closed during preparation"));
        }
    }
}

pub(super) fn known_submission(state: &CoreAgentState, command: &CoreAgentCommand) -> bool {
    let CoreAgentCommand::RequestRun(request) = command else {
        return false;
    };
    let Some(id) = &request.submission_id else {
        return false;
    };
    state
        .runs
        .active
        .as_ref()
        .is_some_and(|run| run.submission_id.as_ref() == Some(id))
        || state
            .runs
            .queued
            .iter()
            .any(|run| run.submission_id.as_ref() == Some(id))
        || state
            .runs
            .completed
            .iter()
            .any(|run| run.submission_id.as_ref() == Some(id))
}

pub(super) fn failure(
    command: &CoreAgentCommand,
    correlation_token: Option<String>,
    error: AgentApiError,
) -> AgentAdmissionFailure {
    AgentAdmissionFailure {
        submission_id: drive::command_submission_id(command),
        correlation_token,
        kind: AgentAdmissionFailureKind::RejectedCommand,
        message: error.message.clone(),
        rejection: None,
        preparation_error: Some(error),
    }
}

pub(super) fn preparation_matches(
    prepared: &SessionToolsetPreparation,
    state: &CoreAgentState,
    universe_id: uuid::Uuid,
) -> bool {
    if prepared.source.matches(state) {
        return true;
    }
    // A preceding observation may already have installed these same immutable
    // system bindings. This does not invalidate the desired tool observation.
    let mut source = prepared.source.clone();
    for declaration in &prepared.declarations {
        let Ok(binding) = engine::WorkflowToolBinding::admit(
            universe_id,
            declaration.definition.clone(),
            declaration.target.clone(),
            declaration.completion.clone(),
        ) else {
            return false;
        };
        source
            .system_binding_ids
            .insert(binding.definition.tool_id.clone());
        source
            .bindings
            .insert(binding.definition.tool_id.clone(), binding);
    }
    source.matches(state)
}

pub(super) async fn publish_tools(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    prepared: SessionToolsetPreparation,
) -> Result<(), AgentApiError> {
    if admissions::turn_in_flight(drive.state()) {
        return Err(AgentApiError::conflict(
            "cannot publish tools during an in-flight turn",
        ));
    }
    let universe_id = ctx
        .state(|state| state.universe_id)
        .ok_or_else(|| AgentApiError::internal("session universe is missing"))?;
    if !preparation_matches(&prepared, drive.state(), universe_id) {
        return Err(AgentApiError::conflict(
            "tool preparation no longer matches session configuration",
        ));
    }
    for declaration in prepared.declarations {
        apply(
            ctx,
            drive,
            CoreAgentCommand::AdmitSystemWorkflowTool {
                session_universe_id: universe_id,
                declaration,
            },
        )
        .await?;
    }
    let patch = crate::session_toolset_patch(&drive.state().tooling.tools, &prepared.tools);
    if !patch.is_empty() {
        apply(
            ctx,
            drive,
            CoreAgentCommand::PatchTools {
                expected_revision: Some(drive.state().tooling.revision),
                patch,
            },
        )
        .await?;
    }
    Ok(())
}

pub(super) struct PendingToolset {
    pub prepared: SessionToolsetPreparation,
    pub run_id: engine::RunId,
    pub admission: AgentAdmission,
}

pub(super) async fn publish_pending_tools(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
) -> anyhow::Result<bool> {
    if admissions::turn_in_flight(drive.state()) {
        return Ok(false);
    }
    let pending = ctx.state_mut(|state| std::mem::take(&mut state.pending_toolsets));
    let changed = !pending.is_empty();
    for pending in pending {
        if drive.state().lifecycle.status != CoreAgentStatus::Open {
            break;
        }
        if let Err(error) = publish_tools(ctx, drive, pending.prepared).await {
            // An accepted run must never execute using an obsolete observation.
            // Reject this submission and cancel its queued work, keeping the
            // session and unrelated runs usable.
            record_admission_failure(
                ctx,
                failure(
                    &pending.admission.command,
                    pending.admission.correlation_token,
                    error,
                ),
            );
            apply(
                ctx,
                drive,
                CoreAgentCommand::CancelRun {
                    run_id: pending.run_id,
                },
            )
            .await?;
        }
    }
    Ok(changed)
}

pub(super) async fn apply(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    command: CoreAgentCommand,
) -> Result<(), AgentApiError> {
    match admit_and_append_command(ctx, drive, command, None)
        .await
        .map_err(|e| AgentApiError::internal(e.to_string()))?
    {
        CommandAdmissionResult::Accepted => Ok(()),
        CommandAdmissionResult::Rejected(failure) => Err(admission_error(failure)),
    }
}

fn admission_error(failure: AgentAdmissionFailure) -> AgentApiError {
    if let Some(error) = failure.preparation_error {
        return error;
    }
    if failure
        .rejection
        .as_ref()
        .is_some_and(|rejection| rejection.kind == engine::CommandRejectionKind::RevisionConflict)
    {
        AgentApiError::conflict(failure.message)
    } else {
        AgentApiError::rejected(failure.message)
    }
}

pub(super) async fn process_operation(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    request: SessionOperationRequest,
) -> anyhow::Result<()> {
    let receipt = request.receipt().expect("session operation serialization");
    // Lookup reports both conflicting IDs and expired retries to the caller;
    // neither may execute or create another retained admission failure.
    if !matches!(
        ctx.state(|state| state.operation_outcomes.lookup(&receipt)),
        Ok(None)
    ) {
        return Ok(());
    }
    let result = if !ctx.state(|state| state.ready) {
        Err(AgentApiError::rejected("session setup has not completed"))
    } else {
        execute_operation(ctx, drive, request.operation).await?
    };
    ctx.state_mut(|state| {
        state
            .operation_outcomes
            .insert(SessionOperationOutcome { receipt, result });
    });
    Ok(())
}

pub(super) async fn execute_operation(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    operation: SessionOperation,
) -> anyhow::Result<Result<ProfileApplySummary, AgentApiError>> {
    let prepared = prepare_operation(ctx, drive, operation).await;
    let (candidate, summary) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return Ok(Err(error)),
    };
    let request = match candidate.finish(drive) {
        Ok(request) => request,
        Err(error) => return Ok(Err(error)),
    };
    // Storage confirms exact-batch retries after a lost response. A commit
    // failure is not evidence of rejection: propagate it without a receipt.
    drive::append_events(ctx, drive, request.expected_head, request.events).await?;
    Ok(Ok(summary))
}

async fn prepare_operation(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    operation: SessionOperation,
) -> Result<(PreparationCandidate, ProfileApplySummary), AgentApiError> {
    let mut candidate = PreparationCandidate::new(drive);
    if drive.state().lifecycle.status != CoreAgentStatus::Open {
        return Err(AgentApiError::rejected("session is not open"));
    }
    let busy = drive.state().runs.active.is_some() || !drive.state().runs.queued.is_empty();
    if matches!(operation, SessionOperation::RefreshContext) && busy {
        return Ok((candidate, ProfileApplySummary::default()));
    }
    if busy || drive.state().context.pending_compaction {
        return Err(AgentApiError::rejected(
            "session preparation requires no active or queued work",
        ));
    }
    let mut summary = ProfileApplySummary::default();
    match operation {
        SessionOperation::Configure {
            config,
            expected_revision,
        } => {
            if expected_revision
                .is_some_and(|revision| revision != drive.state().lifecycle.config_revision)
            {
                return Err(AgentApiError::conflict(
                    "session configuration revision changed",
                ));
            }
            summary.config_changed = drive.state().lifecycle.config.as_ref() != Some(&config);
            configure(ctx, drive, &mut candidate, config, expected_revision).await?;
        }
        SessionOperation::ApplyProfile {
            profile,
            expected_config_revision,
            expected_tools_revision,
        } => {
            if expected_config_revision
                .is_some_and(|revision| revision != drive.state().lifecycle.config_revision)
                || expected_tools_revision
                    .is_some_and(|revision| revision != drive.state().tooling.revision)
            {
                return Err(AgentApiError::conflict(
                    "session configuration or tool revision changed",
                ));
            }
            summary = apply_profile(
                ctx,
                drive,
                &mut candidate,
                profile,
                expected_config_revision,
            )
            .await?;
        }
        SessionOperation::RefreshContext => {}
    }
    let request = admissions::runtime_projection_request(drive.session_id(), candidate.state());
    let commands = admissions::prepare_runtime_projection(ctx, drive, request)
        .await
        .map_err(|error| AgentApiError::internal(error.to_string()))?;
    for command in commands {
        candidate.push(command, workflow_time_ms(ctx))?;
    }
    Ok((candidate, summary))
}

async fn configure(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    candidate: &mut PreparationCandidate,
    config: engine::SessionConfig,
    expected_revision: Option<u64>,
) -> Result<(), AgentApiError> {
    // Validate materialization before committing configuration. Registry reads
    // belong to this activity; the engine remains the final state validator.
    let original = SessionToolsetSource::from_state(drive.state()).unwrap();
    let mut source = original.clone();
    source.config = config.clone();
    let activity_ctx = ctx.clone();
    let activity = activity_ctx.start_activity(
        WorkflowActivities::prepare_session_toolset,
        crate::SessionToolsetRequest {
            source: source.clone(),
            validate_configuration: true,
        },
        activity_options(),
    );
    let prepared = await_activity(ctx, drive, activity)
        .await?
        .map_err(|e| AgentApiError::internal(format!("configuration preparation failed: {e}")))??;
    if prepared.source != source || !original.matches(drive.state()) {
        return Err(AgentApiError::conflict(
            "configuration changed during preparation",
        ));
    }
    candidate.tools(
        prepared,
        ctx.state(|state| state.universe_id).unwrap(),
        workflow_time_ms(ctx),
    )?;
    candidate.push(
        CoreAgentCommand::ReplaceSessionConfig {
            expected_revision,
            config,
        },
        workflow_time_ms(ctx),
    )
}

async fn apply_profile(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    candidate: &mut PreparationCandidate,
    profile: crate::SessionProfileIntent,
    expected_revision: Option<u64>,
) -> Result<ProfileApplySummary, AgentApiError> {
    let config = profile
        .config
        .clone()
        .or_else(|| drive.state().lifecycle.config.clone())
        .unwrap();
    let mut summary = ProfileApplySummary::default();
    let original = SessionToolsetSource::from_state(drive.state()).unwrap();
    let mut source = original.clone();
    source.config = config.clone();
    let request = crate::SessionProfilePreparationRequest {
        session_id: drive.session_id().clone(),
        instructions: profile.instructions,
        environment: profile.environment,
        source: source.clone(),
    };
    let activity_ctx = ctx.clone();
    let activity = activity_ctx.start_activity(
        WorkflowActivities::prepare_session_profile,
        request,
        activity_options(),
    );
    let prepared = await_activity(ctx, drive, activity)
        .await?
        .map_err(|e| AgentApiError::internal(format!("profile preparation failed: {e}")))??;
    if prepared.toolset.source != source || !original.matches(drive.state()) {
        return Err(AgentApiError::conflict(
            "session changed during profile preparation",
        ));
    }
    candidate.tools(
        prepared.toolset,
        ctx.state(|state| state.universe_id).unwrap(),
        workflow_time_ms(ctx),
    )?;
    if profile.config.is_some() {
        summary.config_changed = drive.state().lifecycle.config.as_ref() != Some(&config);
        candidate.push(
            CoreAgentCommand::ReplaceSessionConfig {
                expected_revision,
                config,
            },
            workflow_time_ms(ctx),
        )?;
    }
    if let Some(environment_id) = prepared.environment_id {
        summary.active_environment_changed =
            candidate.state().environment.active_environment_id.as_ref() != Some(&environment_id);
        candidate.push(
            CoreAgentCommand::SetActiveEnvironment { environment_id },
            workflow_time_ms(ctx),
        )?;
    }
    let mut desired = admissions::active_instruction_inputs(candidate.state());
    desired.retain(|key, _| {
        key.as_str() != "instructions.050.profile"
            && !key.as_str().starts_with("instructions.050.profile.")
    });
    desired.extend(prepared.instructions);
    desired.remove(&ContextEntryKey::new("instructions.000.default"));
    if desired.is_empty() {
        let activity_ctx = ctx.clone();
        let activity = activity_ctx.start_activity(
            WorkflowActivities::put_blob,
            PutBlobRequest {
                bytes: default_instructions().as_bytes().to_vec(),
            },
            activity_options(),
        );
        let reference = await_activity(ctx, drive, activity)
            .await?
            .map_err(|e| AgentApiError::internal(e.to_string()))?;
        desired.insert(
            ContextEntryKey::new("instructions.000.default"),
            ContextEntryInput {
                kind: ContextEntryKind::Instructions,
                content: engine::ContentRef::text(reference),
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            },
        );
    }
    summary.instructions_changed =
        desired != admissions::active_instruction_inputs(candidate.state());
    if summary.instructions_changed {
        candidate.push(
            CoreAgentCommand::ReplaceContextPrefix {
                expected_revision: Some(candidate.state().context.revision),
                key_prefix: ContextEntryKey::new("instructions"),
                entries: desired,
            },
            workflow_time_ms(ctx),
        )?;
    }
    Ok(summary)
}

pub(super) const PREPARATION_PATCH: &str = "workflow_owned_session_preparation_v1";

pub(super) async fn prepare_initial_session(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> anyhow::Result<()> {
    if !ctx.patched(PREPARATION_PATCH) {
        // Existing histories performed setup through gateway commands. Replay
        // those commands without inserting new activities. At new history the
        // marker enables preparation for subsequent submissions.
        ctx.state_mut(|state| {
            state.ready = true;
            state.setup_requested = false;
        });
        return Ok(());
    }
    if ctx.state(|state| state.ready || !state.setup_requested) {
        return Ok(());
    }
    ctx.state_mut(|state| {
        state.setup_requested = false;
        state.setup_error = None;
    });
    let mut drive = drive_from_state(ctx)?;
    let operation = match &args.setup {
        Some(profile) => SessionOperation::ApplyProfile {
            profile: profile.clone(),
            expected_config_revision: None,
            expected_tools_revision: None,
        },
        None => SessionOperation::Configure {
            config: drive.state().lifecycle.config.clone().unwrap(),
            expected_revision: None,
        },
    };
    let result = execute_operation(ctx, &mut drive, operation)
        .await?
        .map(|_| ());
    ctx.state_mut(|state| match result {
        Ok(()) => {
            state.ready = true;
            state.setup_error = None;
        }
        Err(error) => state.setup_error = Some(error),
    });
    Ok(())
}

/// The observation intent is workflow state; its activity future stays in the
/// concurrent preparation loop so the workflow's public handle remains Send.
#[derive(Clone)]
pub(super) struct PendingRunPreparation {
    admission: AgentAdmission,
    source: SessionToolsetSource,
}

pub(super) fn begin_run_preparation(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    drive: &CoreAgentDrive,
    admission: AgentAdmission,
) {
    let source = SessionToolsetSource::from_state(drive.state()).expect("open session config");
    ctx.state_mut(|state| {
        debug_assert!(state.run_preparation.is_none());
        state.run_preparation = Some(PendingRunPreparation { admission, source });
    });
}

/// Poll policy reads alongside the session driver, including while its model
/// or tool activity is in flight. Only the driver publishes the observation.
pub(super) async fn run_preparation_loop(ctx: WorkflowContext<AgentSessionWorkflow>) {
    loop {
        ctx.wait_condition(|state| state.run_preparation.is_some())
            .await;
        let pending = ctx.state(|state| state.run_preparation.clone()).unwrap();
        let activity = ctx.start_activity(
            WorkflowActivities::prepare_session_toolset,
            crate::SessionToolsetRequest {
                source: pending.source,
                validate_configuration: false,
            },
            activity_options(),
        );
        let abandoned = ctx.wait_condition(|state| state.run_preparation.is_none());
        pin_mut!(activity, abandoned);
        let result = futures::select_biased! {
            result = activity => result.map_err(|error| AgentApiError::internal(format!("tool preparation failed: {error}"))).and_then(|result| result),
            _ = abandoned => {
                activity.as_ref().get_ref().cancel();
                let _ = activity.await;
                continue;
            }
        };
        ctx.state_mut(|state| {
            if let Some(pending) = state.run_preparation.take() {
                state.pending_admissions.insert(
                    0,
                    SessionAdmission::PreparedRun {
                        admission: pending.admission,
                        result,
                    },
                );
            }
        });
    }
}

pub(super) fn admission_can_pass_preparation(admission: &SessionAdmission) -> bool {
    control_admission(admission)
}

pub(super) fn abandon_pending_run(state: &mut AgentSessionWorkflow) {
    if let Some(pending) = state.run_preparation.take() {
        state.admission_failures.push(failure(
            &pending.admission.command,
            pending.admission.correlation_token,
            AgentApiError::rejected("session closed during tool preparation"),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending() -> PendingRunPreparation {
        let config = crate::default_session_config(engine::ModelSelection {
            api_kind: engine::ProviderApiKind::OpenAiResponses,
            provider_id: "openai".into(),
            model: "gpt-test".into(),
        });
        let mut state = CoreAgentState::new();
        state.lifecycle.config = Some(config);
        PendingRunPreparation {
            admission: AgentAdmission {
                command: CoreAgentCommand::RequestRun(engine::RunRequestCommand {
                    submission_id: Some(SubmissionId::new("prepared")),
                    source: engine::RunRequestSource::Input { input: Vec::new() },
                    run_config: crate::default_run_config(),
                    notify_on_terminal: Vec::new(),
                }),
                correlation_token: Some("receipt".into()),
            },
            source: SessionToolsetSource::from_state(&state).unwrap(),
        }
    }

    #[test]
    fn slow_preparation_keeps_controls_admissible_and_blocks_later_mutations() {
        let pending = pending();
        let mut state = AgentSessionWorkflow {
            ready: true,
            setup_requested: false,
            ..Default::default()
        };
        state.core_state.lifecycle.config = Some(pending.source.config.clone());
        state.run_preparation = Some(pending);
        state.queue_admission(AgentAdmission {
            command: CoreAgentCommand::ReplaceSessionConfig {
                config: crate::default_session_config(engine::ModelSelection {
                    api_kind: engine::ProviderApiKind::OpenAiResponses,
                    provider_id: "openai".into(),
                    model: "gpt-test".into(),
                }),
                expected_revision: None,
            },
            correlation_token: None,
        });
        assert!(!admissions::has_admissible_admissions(&state));
        assert!(!wait_loop::workflow_state_has_immediate_work(&state));
        assert!(!wait_loop::workflow_state_allows_continue_as_new(&state));
        assert_eq!(state.status_snapshot().pending_admissions, 2);
        state.queue_admission(AgentAdmission {
            command: CoreAgentCommand::CancelRun {
                run_id: engine::RunId::new(1),
            },
            correlation_token: None,
        });
        assert!(admissions::has_admissible_admissions(&state));
        assert!(wait_loop::workflow_state_has_immediate_work(&state));
    }

    #[test]
    fn closing_pending_preparation_reports_its_receipt_without_poisoning_session() {
        let mut state = AgentSessionWorkflow {
            run_preparation: Some(pending()),
            ..Default::default()
        };
        abandon_pending_run(&mut state);
        assert!(state.run_preparation.is_none());
        assert_eq!(state.admission_failures.len(), 1);
        let failure = &state.admission_failures[0];
        assert_eq!(failure.submission_id, Some(SubmissionId::new("prepared")));
        assert_eq!(failure.correlation_token.as_deref(), Some("receipt"));
        assert!(failure.preparation_error.is_some());
        assert!(state.last_error.is_none());
        abandon_pending_run(&mut state);
        assert_eq!(state.admission_failures.len(), 1);
    }

    #[test]
    fn unfinished_setup_cannot_drive_or_continue_as_new() {
        let mut state = AgentSessionWorkflow {
            setup_requested: false,
            ..Default::default()
        };
        state.core_state.context.pending_compaction = true;
        assert!(!wait_loop::workflow_state_needs_core_drive_for_state(
            &state
        ));
        assert!(!wait_loop::workflow_state_allows_continue_as_new(&state));
        state.ready = true;
        assert!(wait_loop::workflow_state_needs_core_drive_for_state(&state));
    }

    #[test]
    fn observation_rejects_changed_configuration_and_system_ownership() {
        let pending = pending();
        let mut state = CoreAgentState::new();
        state.lifecycle.config = Some(pending.source.config.clone());
        assert!(pending.source.matches(&state));
        state.lifecycle.config_revision += 1;
        assert!(!pending.source.matches(&state));
        state.lifecycle.config_revision -= 1;
        state
            .workflow_tools
            .system_binding_ids
            .insert(engine::WorkflowToolId::new("foreign"));
        assert!(!pending.source.matches(&state));
    }
}
