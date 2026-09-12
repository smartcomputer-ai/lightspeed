use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RolloverReason {
    ServerSuggested,
    HistoryThreshold,
}

impl RolloverReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ServerSuggested => "server_suggested",
            Self::HistoryThreshold => "history_threshold",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RolloverBlockers {
    pub awaiting_safe_checkpoint: bool,
    pub pending_admissions: usize,
    pub pending_preparations: usize,
    pub pending_tool_batch_resumes: usize,
    pub pending_emissions: usize,
    pub pending_source_resolutions: usize,
    pub pending_promise_cancellations: usize,
    pub workflow_start_backoffs: usize,
    pub cancellation_watchdog: bool,
}

impl RolloverBlockers {
    fn from_state(state: &AgentSessionWorkflow, awaiting_safe_checkpoint: bool) -> Self {
        Self {
            awaiting_safe_checkpoint,
            pending_admissions: state.pending_admissions.len(),
            pending_preparations: usize::from(state.run_preparation.is_some())
                + state.pending_toolsets.len()
                + usize::from(!state.ready),
            pending_tool_batch_resumes: state.pending_tool_batch_resumes.len(),
            pending_emissions: state.pending_emissions.len(),
            pending_source_resolutions: state.pending_source_resolutions.len(),
            pending_promise_cancellations: state.pending_promise_cancellations.len(),
            workflow_start_backoffs: state.workflow_start_backoffs.len(),
            cancellation_watchdog: state.cancelling_watchdog.is_some(),
        }
    }

    fn is_empty(self) -> bool {
        !self.awaiting_safe_checkpoint
            && self.pending_admissions == 0
            && self.pending_preparations == 0
            && self.pending_tool_batch_resumes == 0
            && self.pending_emissions == 0
            && self.pending_source_resolutions == 0
            && self.pending_promise_cancellations == 0
            && self.workflow_start_backoffs == 0
            && !self.cancellation_watchdog
    }
}

pub(super) fn request_continue_as_new(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> WorkflowResult<()> {
    let next_args = continuation_args(ctx, args);
    match ctx.continue_as_new(&next_args, ContinueAsNewOptions::default()) {
        Ok(never) => match never {},
        Err(termination @ temporalio_sdk::WorkflowTermination::ContinueAsNew(_)) => {
            log_continue_as_new(ctx, args);
            Err(termination)
        }
        Err(termination @ temporalio_sdk::WorkflowTermination::Failed(_)) => {
            log_workflow_failure(
                ctx,
                "continue_as_new_serialization",
                &termination.to_string(),
            );
            Err(termination)
        }
        Err(termination) => Err(termination),
    }
}

pub(super) fn observe_rollover_delay(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) {
    if rollover_reason(ctx, args).is_none() || ctx.state(|state| state.rollover_delay_logged) {
        return;
    }

    // The normal continue-as-new decision runs before this observer. Do not
    // call `ctx.patched` here: diagnostics must not alter patch-marker command
    // ordering for existing workflow histories.
    let blockers = ctx.state(|state| {
        let awaiting_safe_checkpoint = wait_loop::workflow_state_allows_continue_as_new(state)
            && !state.execution_has_rollover_checkpoint;
        RolloverBlockers::from_state(state, awaiting_safe_checkpoint)
    });
    if blockers.is_empty() {
        return;
    }

    ctx.state_mut(|state| state.rollover_delay_logged = true);
    if ctx.is_replaying() {
        return;
    }

    let fields = session_fields(ctx);
    tracing::warn!(
        target: "temporal_workflow",
        event = "session_rollover_delayed",
        universe_id = fields.universe_id,
        session_id = fields.session_id,
        workflow_id = ctx.workflow_id(),
        temporal_run_id = ctx.run_id(),
        lightspeed_run_id = fields.active_run_id,
        session_head_seq = fields.session_head_seq,
        history_length = ctx.history_length(),
        history_threshold = rollover_threshold(args),
        awaiting_safe_checkpoint = blockers.awaiting_safe_checkpoint,
        pending_admissions = blockers.pending_admissions,
        pending_preparations = blockers.pending_preparations,
        pending_tool_batch_resumes = blockers.pending_tool_batch_resumes,
        pending_emissions = blockers.pending_emissions,
        pending_source_resolutions = blockers.pending_source_resolutions,
        pending_promise_cancellations = blockers.pending_promise_cancellations,
        workflow_start_backoffs = blockers.workflow_start_backoffs,
        cancellation_watchdog = blockers.cancellation_watchdog,
        "session history rollover is delayed"
    );
}

pub(super) fn log_workflow_failure(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    error_class: &'static str,
    error: &str,
) {
    if ctx.is_replaying() {
        return;
    }
    let fields = session_fields(ctx);
    tracing::error!(
        target: "temporal_workflow",
        event = "session_workflow_failed",
        universe_id = fields.universe_id,
        session_id = fields.session_id,
        workflow_id = ctx.workflow_id(),
        temporal_run_id = ctx.run_id(),
        lightspeed_run_id = fields.active_run_id,
        session_head_seq = fields.session_head_seq,
        history_length = ctx.history_length(),
        error_class,
        error,
        "session workflow failed"
    );
}

fn log_continue_as_new(ctx: &WorkflowContext<AgentSessionWorkflow>, args: &AgentSessionArgs) {
    if ctx.is_replaying() {
        return;
    }
    let fields = session_fields(ctx);
    let reason = rollover_reason(ctx, args)
        .expect("continue-as-new is only requested when history rollover is due");
    let admission_failure_count = ctx.state(|state| state.admission_failures.len());
    tracing::info!(
        target: "temporal_workflow",
        event = "session_continue_as_new",
        universe_id = fields.universe_id,
        session_id = fields.session_id,
        workflow_id = ctx.workflow_id(),
        temporal_run_id = ctx.run_id(),
        lightspeed_run_id = fields.active_run_id,
        session_head_seq = fields.session_head_seq,
        reason = reason.as_str(),
        history_length = ctx.history_length(),
        history_threshold = rollover_threshold(args),
        admission_failure_count,
        "session continuing as new"
    );
}

pub(super) fn rollover_reason(
    ctx: &WorkflowContext<AgentSessionWorkflow>,
    args: &AgentSessionArgs,
) -> Option<RolloverReason> {
    rollover_reason_for(
        ctx.continue_as_new_suggested(),
        ctx.history_length(),
        rollover_threshold(args),
    )
}

fn rollover_reason_for(
    server_suggested: bool,
    history_length: u32,
    history_threshold: u32,
) -> Option<RolloverReason> {
    if server_suggested {
        Some(RolloverReason::ServerSuggested)
    } else if history_length >= history_threshold {
        Some(RolloverReason::HistoryThreshold)
    } else {
        None
    }
}

fn rollover_threshold(args: &AgentSessionArgs) -> u32 {
    args.continue_as_new_history_threshold
        .unwrap_or(DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD)
}

struct SessionFields {
    universe_id: String,
    session_id: String,
    active_run_id: String,
    session_head_seq: String,
}

fn session_fields(ctx: &WorkflowContext<AgentSessionWorkflow>) -> SessionFields {
    ctx.state(|state| {
        let workflow_identity = split_workflow_id(ctx.workflow_id());
        let universe_id = state
            .universe_id
            .or_else(|| {
                workflow_identity
                    .as_ref()
                    .map(|(universe_id, _)| *universe_id)
            })
            .map(|id| id.to_string())
            .unwrap_or_default();
        let session_id = state
            .session_id
            .as_ref()
            .or_else(|| workflow_identity.as_ref().map(|(_, session_id)| session_id))
            .map(ToString::to_string)
            .unwrap_or_default();
        let active_run_id = state
            .core_state
            .runs
            .active
            .as_ref()
            .map(|run| run.run_id.to_string())
            .unwrap_or_default();
        let session_head_seq = state
            .head
            .as_ref()
            .map(|head| head.seq.to_string())
            .unwrap_or_default();
        SessionFields {
            universe_id,
            session_id,
            active_run_id,
            session_head_seq,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_suggestion_has_reason_precedence() {
        assert_eq!(
            rollover_reason_for(true, 10_000, 10_000),
            Some(RolloverReason::ServerSuggested)
        );
    }

    #[test]
    fn history_threshold_reason_requires_threshold() {
        assert_eq!(
            rollover_reason_for(false, 10_000, 10_000),
            Some(RolloverReason::HistoryThreshold)
        );
        assert_eq!(rollover_reason_for(false, 9_999, 10_000), None);
    }

    #[test]
    fn rollover_blockers_report_transient_workflow_state() {
        let mut state = AgentSessionWorkflow::default();
        state.queue_admission(AgentAdmission {
            command: CoreAgentCommand::CloseSession { force: false },
            correlation_token: None,
        });
        let blockers = RolloverBlockers::from_state(&state, false);

        assert!(!blockers.awaiting_safe_checkpoint);
        assert_eq!(blockers.pending_admissions, 1);
        assert!(!blockers.is_empty());

        let checkpoint = RolloverBlockers::from_state(&AgentSessionWorkflow::default(), true);
        assert!(checkpoint.awaiting_safe_checkpoint);
        assert!(!checkpoint.is_empty());
    }
}
