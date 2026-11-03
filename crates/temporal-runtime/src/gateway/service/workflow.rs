use super::*;

impl GatewayAgentApi {
    /// Universe (tenant) this gateway instance serves, taken from the bound
    /// store. All Temporal addressing composes it into the workflow id.
    pub fn universe_id(&self) -> uuid::Uuid {
        self.store.config().universe_id
    }

    pub(crate) fn workflow_id_for(&self, session_id: &SessionId) -> String {
        temporal_workflow::compose_workflow_id(self.universe_id(), session_id)
    }

    pub(super) fn require_universe_workflow_reference(
        &self,
        label: &str,
        workflow_id: &str,
    ) -> Result<(), AgentApiError> {
        require_universe_workflow_reference(self.universe_id(), label, workflow_id)
    }

    pub(super) fn workflow_handle(
        &self,
        session_id: &SessionId,
    ) -> WorkflowHandle<Client, AgentSessionWorkflow> {
        self.client
            .get_workflow_handle::<AgentSessionWorkflow>(self.workflow_id_for(session_id))
    }

    pub(super) async fn submit_core_command(
        &self,
        session_id: &SessionId,
        command: CoreAgentCommand,
    ) -> Result<(), AgentApiError> {
        self.submit_core_commands(session_id, vec![command]).await
    }

    /// Encodes commands and signals them as one uncorrelated admission batch.
    pub(super) async fn submit_core_commands(
        &self,
        session_id: &SessionId,
        commands: Vec<CoreAgentCommand>,
    ) -> Result<(), AgentApiError> {
        let admissions = commands
            .into_iter()
            .map(|command| AgentAdmission {
                command,
                correlation_token: None,
            })
            .collect();
        self.signal_submit_admissions(session_id, admissions).await
    }

    /// Submit context commands with one transport-only correlation token per
    /// command. The returned map lets request-local waiters associate an
    /// asynchronous failure with the context key or prefix it belongs to.
    pub(super) async fn submit_correlated_context_commands(
        &self,
        session_id: &SessionId,
        commands: Vec<CoreAgentCommand>,
    ) -> Result<BTreeMap<String, ContextEntryKey>, AgentApiError> {
        let mut correlations = BTreeMap::new();
        let mut admissions = Vec::with_capacity(commands.len());
        for command in commands {
            let context_key = match &command {
                CoreAgentCommand::UpsertContext { key, .. }
                | CoreAgentCommand::RemoveContext { key, .. } => key.clone(),
                CoreAgentCommand::ReplaceContextPrefix { key_prefix, .. } => key_prefix.clone(),
                _ => {
                    return Err(AgentApiError::internal(
                        "correlated context submission received a non-context command",
                    ));
                }
            };
            let correlation_token = format!("admit_{}", uuid::Uuid::new_v4().simple());
            correlations.insert(correlation_token.clone(), context_key);
            admissions.push(AgentAdmission {
                command,
                correlation_token: Some(correlation_token),
            });
        }
        self.signal_submit_admissions(session_id, admissions)
            .await?;
        Ok(correlations)
    }

    /// Signal an encoded admission batch to the session workflow. A raw
    /// Temporal `NotFound` is classified: a workflow that exists but failed at
    /// bootstrap is reported as `session_bootstrap_failed`, not the misleading
    /// "agent workflow not found".
    pub(super) async fn signal_submit_admissions(
        &self,
        session_id: &SessionId,
        admissions: Vec<AgentAdmission>,
    ) -> Result<(), AgentApiError> {
        self.refresh_input_blob_grace(&admissions).await?;
        match self
            .workflow_handle(session_id)
            .signal(
                AgentSessionWorkflow::submit_admissions,
                admissions,
                WorkflowSignalOptions::default(),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(WorkflowInteractionError::NotFound(_)) => Err(self
                .classify_workflow_interaction_not_found(session_id)
                .await),
            Err(error) => Err(map_workflow_interaction_error(error)),
        }
    }

    /// Refresh externally admitted refs in one database statement before the
    /// workflow can queue them. Reads alone do not renew an upload's grace.
    /// Authorization of caller-supplied references happens where the caller's
    /// own document is parsed, before the runtime derives the admission.
    pub(super) async fn refresh_input_blob_grace(
        &self,
        input: &impl serde::Serialize,
    ) -> Result<(), AgentApiError> {
        let refs = harness::storage::typed_blob_refs(input)
            .map_err(|error| AgentApiError::internal(format!("encode admission: {error}")))?;
        self.store
            .touch_blob_refs(&refs.into_iter().collect::<Vec<_>>())
            .await
            .map_err(map_blob_store_error)
    }

    /// Waits for exact context entries to commit; any per-entry admission
    /// failure is escalated to a call-level typed error. Built on the same
    /// wait loop as `session/context/append`.
    pub(super) async fn wait_for_context_append_outcomes(
        &self,
        session_id: &SessionId,
        expected: &[(ContextEntryKey, ContextEntryInput)],
        correlations: &BTreeMap<String, ContextEntryKey>,
    ) -> Result<(u64, BTreeMap<ContextEntryKey, ContextAppendWaitOutcome>), AgentApiError> {
        let started = Instant::now();
        let mut outcomes = BTreeMap::new();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for context entries to apply: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                for failure in &status.admission_failures {
                    let Some(token) = failure.correlation_token.as_ref() else {
                        continue;
                    };
                    let Some(key) = correlations.get(token) else {
                        continue;
                    };
                    outcomes.entry(key.clone()).or_insert_with(|| {
                        ContextAppendWaitOutcome::Failed {
                            failure: failure.clone(),
                        }
                    });
                }
                if let Some(error) = status.fatal_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            for (key, input) in expected {
                if outcomes.contains_key(key) {
                    continue;
                }
                if let Some(active) = matching_current_context_entry(&loaded.state, key, input) {
                    outcomes.insert(
                        key.clone(),
                        ContextAppendWaitOutcome::Applied {
                            entry: active_entry_input(active),
                        },
                    );
                }
            }
            if outcomes.len() == expected.len() {
                return Ok((loaded.state.context.revision, outcomes));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    /// Waits until each key is absent from active context (removed) or its
    /// removal admission failed. Mirrors `wait_for_context_append_outcomes`
    /// with an absence condition instead of an effective-entry match.
    pub(super) async fn wait_for_context_keys_removed(
        &self,
        session_id: &SessionId,
        expected: &[ContextEntryKey],
        correlations: &BTreeMap<String, ContextEntryKey>,
    ) -> Result<
        (
            u64,
            BTreeMap<ContextEntryKey, Option<AgentAdmissionFailure>>,
        ),
        AgentApiError,
    > {
        let started = Instant::now();
        let mut outcomes: BTreeMap<ContextEntryKey, Option<AgentAdmissionFailure>> =
            BTreeMap::new();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for context entries to be removed: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                for failure in &status.admission_failures {
                    let Some(token) = failure.correlation_token.as_ref() else {
                        continue;
                    };
                    let Some(key) = correlations.get(token) else {
                        continue;
                    };
                    outcomes
                        .entry(key.clone())
                        .or_insert_with(|| Some(failure.clone()));
                }
                if let Some(error) = status.fatal_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            for key in expected {
                if outcomes.contains_key(key) {
                    continue;
                }
                let present = loaded
                    .state
                    .context
                    .entries
                    .iter()
                    .any(|entry| entry.key.as_ref() == Some(key));
                if !present {
                    outcomes.insert(key.clone(), None);
                }
            }
            if outcomes.len() == expected.len() {
                return Ok((loaded.state.context.revision, outcomes));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    /// Wait until every entry holds its replacement content, or the workflow
    /// reports the command's admission failure.
    pub(super) async fn wait_for_context_entries_replaced(
        &self,
        session_id: &SessionId,
        expected: &[(harness::ContextEntryId, harness::ContentRef)],
        correlation_token: &str,
    ) -> Result<(u64, Option<AgentAdmissionFailure>), AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for context entries to be replaced: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if let Some(failure) = status
                    .admission_failures
                    .iter()
                    .find(|failure| failure.correlation_token.as_deref() == Some(correlation_token))
                {
                    let failure = failure.clone();
                    let loaded = self.load_session_state(session_id).await?;
                    return Ok((loaded.state.context.revision, Some(failure)));
                }
                if let Some(error) = status.fatal_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            let applied = expected.iter().all(|(entry_id, content)| {
                loaded
                    .state
                    .context
                    .entries
                    .iter()
                    .find(|entry| entry.entry_id == *entry_id)
                    .is_none_or(|entry| entry.content == *content)
            });
            if applied {
                return Ok((loaded.state.context.revision, None));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(super) async fn wait_for_context_compaction_complete(
        &self,
        session_id: &SessionId,
        baseline_revision: u64,
        baseline_failures: usize,
    ) -> Result<SessionView, AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for agent context update: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if status.admission_failures.len() > baseline_failures
                    && let Some(failure) = status.admission_failures.last()
                {
                    return Err(failure.api_error());
                }
                if let Some(error) = status.fatal_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            if loaded
                .state
                .context
                .compaction
                .last_manual_finished_revision
                .is_some_and(|revision| revision > baseline_revision)
                && !loaded.state.context.compaction.is_pending()
                && !loaded.state.context.compaction.is_queued()
            {
                return self.project_session_by_id(session_id).await;
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(super) async fn wait_for_run_accepted(
        &self,
        session_id: &SessionId,
        submission_id: &SubmissionId,
        baseline_failures: usize,
        wait_for_admission_drain: bool,
    ) -> Result<RunView, AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for agent run to start: {submission_id}"
                )));
            }
            let Some(status) = self.query_status_optional(session_id).await? else {
                tokio::time::sleep(self.poll_interval).await;
                continue;
            };
            if let Some(failure) = status
                .admission_failures
                .iter()
                .skip(baseline_failures)
                .rev()
                .find(|failure| failure.submission_id.as_ref() == Some(submission_id))
            {
                return Err(failure.api_error());
            }
            let can_return_matching_run =
                !wait_for_admission_drain || status.pending_admissions == 0;
            if let Some(active) = status
                .active_run
                .as_ref()
                .filter(|run| run.submission_id.as_ref() == Some(submission_id))
                .filter(|_| can_return_matching_run)
            {
                return self
                    .project_run_by_id(session_id, RunId::new(active.run_id))
                    .await;
            }
            if let Some(run) = status
                .completed_runs
                .iter()
                .rev()
                .find(|run| run.submission_id.as_ref() == Some(submission_id))
                .filter(|_| can_return_matching_run)
            {
                return self
                    .project_run_by_id(session_id, RunId::new(run.run_id))
                    .await;
            }
            // A run accepted behind an active run is returned as
            // `queued`; callers follow events for its start.
            if let Some(run) = status
                .queued_runs
                .iter()
                .find(|run| run.submission_id.as_ref() == Some(submission_id))
                .filter(|_| can_return_matching_run)
            {
                return self
                    .project_run_by_id(session_id, RunId::new(run.run_id))
                    .await;
            }
            if let Some(error) = status.fatal_error {
                return Err(AgentApiError::internal(format!(
                    "agent workflow reported error: {error}"
                )));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    /// Wait for a correlated steering admission: resolved when the active
    /// run's steering count advances past `baseline`, failed when the
    /// workflow records a failure for `correlation_token` or the run leaves
    /// the active slot before the steering landed.
    pub(super) async fn wait_for_steering_accepted(
        &self,
        session_id: &SessionId,
        run_id: RunId,
        baseline: usize,
        correlation_token: &str,
    ) -> Result<harness::SteeringId, AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for steering admission: {}",
                    api_run_id(run_id)
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if let Some(failure) =
                    status.admission_failures.iter().rev().find(|failure| {
                        failure.correlation_token.as_deref() == Some(correlation_token)
                    })
                {
                    return Err(failure.api_error());
                }
                if let Some(error) = status.fatal_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            match loaded.state.runs.active.as_ref() {
                Some(active) if active.run_id == run_id => {
                    if active.steering.len() > baseline
                        && let Some(steering) = active.steering.last()
                    {
                        return Ok(steering.steering_id);
                    }
                }
                _ => {
                    return Err(AgentApiError::rejected(format!(
                        "run ended before steering was admitted: {}",
                        api_run_id(run_id)
                    )));
                }
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(super) async fn wait_for_approval_decision(
        &self,
        session_id: &SessionId,
        run_id: RunId,
        approval_id: &harness::ApprovalId,
        correlation_token: &str,
    ) -> Result<(), AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for approval decision: {approval_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if let Some(failure) =
                    status.admission_failures.iter().rev().find(|failure| {
                        failure.correlation_token.as_deref() == Some(correlation_token)
                    })
                {
                    return Err(failure.api_error());
                }
                if let Some(error) = status.fatal_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            if let Some(active_run) = loaded.state.runs.active.as_ref()
                && active_run.run_id == run_id
            {
                // The caller saw this approval pending, so a record that is gone was
                // decided and pruned as the run moved on.
                if active_run
                    .approvals
                    .get(approval_id)
                    .is_none_or(|record| record.status.is_terminal())
                {
                    return Ok(());
                }
            } else if loaded
                .state
                .runs
                .completed
                .iter()
                .any(|record| record.run_id == run_id)
            {
                // The admission was accepted and the run may have continued
                // through completion before this polling read observed it.
                return Ok(());
            } else {
                return Err(AgentApiError::not_found(format!("run not found: {run_id}")));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(super) async fn wait_for_closed_session(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionView, AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for agent session to close: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await?
                && let Some(error) = status.fatal_error
            {
                return Err(AgentApiError::internal(format!(
                    "agent workflow reported error: {error}"
                )));
            }
            let session = self.project_session_by_id(session_id).await?;
            if matches!(session.status, api::SessionStatus::Closed) {
                return Ok(session);
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(super) async fn wait_for_cancelled_run(
        &self,
        session_id: &SessionId,
        run_id: RunId,
    ) -> Result<RunView, AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for agent run cancellation: {}",
                    api_run_id(run_id)
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await?
                && let Some(error) = status.fatal_error
            {
                return Err(AgentApiError::internal(format!(
                    "agent workflow reported error: {error}"
                )));
            }
            let loaded = self.load_session_state(session_id).await?;
            if loaded
                .state
                .runs
                .completed
                .iter()
                .any(|run| run.run_id == run_id)
            {
                return self.project_run_by_id(session_id, run_id).await;
            }
            // A parked run is still live; only `cancelling` (or a terminal
            // record above) means the cancel landed.
            if loaded
                .state
                .runs
                .active
                .as_ref()
                .is_some_and(|run| run.run_id == run_id && run.status == RunStatus::Cancelling)
            {
                return self.project_run_by_id(session_id, run_id).await;
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(super) async fn query_status_optional(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<AgentSessionStatus>, AgentApiError> {
        let handle = self.workflow_handle(session_id);
        match handle
            .query(
                AgentSessionWorkflow::status,
                (),
                WorkflowQueryOptions::default(),
            )
            .await
        {
            Ok(status) => {
                // A queryable workflow that reports a bootstrap failure is a
                // session recovery problem, not a generic internal error.
                if status.bootstrap_failed {
                    return Err(session_bootstrap_failed_error(
                        session_id,
                        status.fatal_error.as_deref(),
                    ));
                }
                Ok(Some(status))
            }
            Err(WorkflowQueryError::NotFound(_)) => Ok(None),
            Err(error) => Err(map_workflow_query_error(error)),
        }
    }

    /// Ask the Temporal server what became of a session's workflow execution.
    /// This is an RPC to the server, so unlike a query it needs no worker.
    pub(crate) async fn execution_liveness(
        &self,
        session_id: &SessionId,
    ) -> Result<ExecutionLiveness, AgentApiError> {
        match self
            .workflow_handle(session_id)
            .describe(WorkflowDescribeOptions::default())
            .await
        {
            Ok(description) => Ok(ExecutionLiveness::from_status(description.status())),
            Err(WorkflowInteractionError::NotFound(_)) => Ok(ExecutionLiveness::Missing),
            Err(error) => Err(map_workflow_interaction_error(error)),
        }
    }

    /// How an open session's execution stopped, for `session/read`. The read
    /// is served from the store, so a server that cannot answer leaves it
    /// without the check instead of failing it.
    pub(super) async fn open_session_execution_failure(
        &self,
        session_id: &SessionId,
        status: api::SessionStatus,
    ) -> Option<api::SessionExecutionFailureView> {
        match self.execution_liveness(session_id).await {
            Ok(liveness) => liveness.open_session_failure(status),
            Err(error) => {
                tracing::warn!(
                    target: "temporal_runtime",
                    session_id = %session_id,
                    error = %error,
                    "could not check the session's workflow execution"
                );
                None
            }
        }
    }

    /// Distinguish a workflow that does not exist from one that exists but is no
    /// longer running (e.g. failed during bootstrap and closed). Used to turn a
    /// raw `NotFound` from a signal/query into a typed
    /// `session_bootstrap_failed` recovery error instead of the misleading
    /// "agent workflow not found".
    pub(super) async fn classify_workflow_interaction_not_found(
        &self,
        session_id: &SessionId,
    ) -> AgentApiError {
        let liveness = match self.execution_liveness(session_id).await {
            Ok(liveness) => liveness,
            Err(error) => return error,
        };
        // Running but the interaction missed it: keep not-found semantics; the
        // caller will typically retry/poll.
        if liveness == ExecutionLiveness::Running {
            return AgentApiError::not_found("agent workflow not found");
        }
        let row = self
            .load_session_state(session_id)
            .await
            .map(|loaded| loaded.state.lifecycle.status);
        stopped_execution_not_found_error(session_id, row, liveness)
    }

    /// Force-close recovery path for sessions with no running workflow:
    /// admits `CloseSession { force: true }` against replayed state and
    /// appends the resulting events (force-cancel active run, drop queued
    /// runs, close) directly to the session store. Status is a projection of
    /// the log, so this alone reconciles the session row; the expected-head
    /// CAS makes a race with any concurrent writer fail here rather than
    /// corrupt the log.
    pub(super) async fn force_close_session_in_store(
        &self,
        session_id: &SessionId,
    ) -> Result<(), AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        if loaded.state.lifecycle.status == CoreAgentStatus::Closed {
            return Ok(());
        }
        let mut drive = harness::CoreAgentDrive::from_replayed(session_id.clone(), loaded.state);
        let action = drive
            .admit_command(
                CoreAgentCommand::CloseSession { force: true },
                u64::try_from(
                    now_ms().map_err(|error| AgentApiError::internal(error.to_string()))?,
                )
                .map_err(|error| AgentApiError::internal(error.to_string()))?,
            )
            .map_err(|error| match error {
                harness::CoreAgentDriveError::Command(harness::CommandError::Rejected(
                    rejection,
                )) => AgentApiError::rejected(rejection.to_string()),
                other => AgentApiError::internal(format!("force close admission failed: {other}")),
            })?;
        match action {
            harness::CoreAgentAction::AppendEvents {
                expected_head,
                events,
            } => {
                self.store
                    .append(harness::storage::AppendSessionEvents {
                        session_id: session_id.clone(),
                        expected_head,
                        events,
                    })
                    .await
                    .map_err(map_session_store_error)?;
                Ok(())
            }
            harness::CoreAgentAction::Idle | harness::CoreAgentAction::Closed => Ok(()),
            other => Err(AgentApiError::internal(format!(
                "force close produced unexpected action: {other:?}"
            ))),
        }
    }
}

/// What the Temporal server knows about a session's workflow execution, by
/// workflow id: the latest run, across rollovers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExecutionLiveness {
    Running,
    Terminal(ExecutionTerminalKind),
    /// No execution exists, or a finished one has passed namespace retention.
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExecutionTerminalKind {
    Completed,
    Failed,
    Canceled,
    Terminated,
    TimedOut,
}

impl ExecutionLiveness {
    fn from_status(status: WorkflowExecutionStatus) -> Self {
        match status {
            WorkflowExecutionStatus::Completed => Self::Terminal(ExecutionTerminalKind::Completed),
            WorkflowExecutionStatus::Failed => Self::Terminal(ExecutionTerminalKind::Failed),
            WorkflowExecutionStatus::Canceled => Self::Terminal(ExecutionTerminalKind::Canceled),
            WorkflowExecutionStatus::Terminated => {
                Self::Terminal(ExecutionTerminalKind::Terminated)
            }
            WorkflowExecutionStatus::TimedOut => Self::Terminal(ExecutionTerminalKind::TimedOut),
            // Only a status that says the execution ended counts as ended. A
            // rollover has a running successor and a pause can be undone.
            _ => Self::Running,
        }
    }

    /// The failure to report for a session the store still has open. Closing a
    /// session is the only clean way out of its workflow, so an execution that
    /// completed or was canceled under an open row stopped short of that.
    fn open_session_failure(
        self,
        status: api::SessionStatus,
    ) -> Option<api::SessionExecutionFailureView> {
        use api::SessionExecutionFailureView as View;
        match self {
            Self::Running => None,
            // A row nothing has opened is not dead for having no execution: a
            // sub-agent's row is written before its workflow starts, and a
            // retried start still opens one.
            Self::Missing if status == api::SessionStatus::NotLoaded => None,
            Self::Missing => Some(View::Missing),
            Self::Terminal(ExecutionTerminalKind::Failed) => Some(View::Failed),
            Self::Terminal(ExecutionTerminalKind::TimedOut) => Some(View::TimedOut),
            Self::Terminal(
                ExecutionTerminalKind::Completed
                | ExecutionTerminalKind::Canceled
                | ExecutionTerminalKind::Terminated,
            ) => Some(View::Terminated),
        }
    }
}

/// The error for an interaction that found no running execution, given what
/// the store says about the row.
fn stopped_execution_not_found_error(
    session_id: &SessionId,
    row: Result<CoreAgentStatus, AgentApiError>,
    liveness: ExecutionLiveness,
) -> AgentApiError {
    match (row, liveness) {
        (Ok(CoreAgentStatus::Closed), _) => {
            AgentApiError::rejected(format!("session is not open: {session_id}"))
        }
        // No row and no execution that failed: the session never existed or
        // was deleted after it ended, which is not a bootstrap problem.
        (Err(error), liveness)
            if is_not_found(&error)
                && liveness != ExecutionLiveness::Terminal(ExecutionTerminalKind::Failed) =>
        {
            error
        }
        (_, ExecutionLiveness::Missing) => AgentApiError::not_found("agent workflow not found"),
        _ => session_bootstrap_failed_error(session_id, None),
    }
}

pub(super) fn session_bootstrap_failed_error(
    session_id: &SessionId,
    reason: Option<&str>,
) -> AgentApiError {
    let detail = reason.unwrap_or("session workflow failed during bootstrap");
    AgentApiError::session_bootstrap_failed(format!(
        "agent session {session_id} failed to start (bootstrap): {detail}"
    ))
}

/// Catalog history retains older entries under the same key. Only the current
/// entry can acknowledge an update; matching a historical version is insufficient.
fn matching_current_context_entry<'a>(
    state: &'a harness::CoreAgentState,
    key: &ContextEntryKey,
    input: &ContextEntryInput,
) -> Option<&'a ContextEntry> {
    harness::current_context_entry(state, key)
        .filter(|entry| active_context_entry_matches_input(entry, input))
}

/// A caller-supplied workflow id that the runtime will signal. Ids in the
/// runtime's own `{universe}/…` namespace name sessions, bots and jobs, so
/// one from another universe is refused; ids outside it belong to plugins.
fn require_universe_workflow_reference(
    universe_id: uuid::Uuid,
    label: &str,
    workflow_id: &str,
) -> Result<(), AgentApiError> {
    let Some((namespace, _)) = workflow_id.split_once('/') else {
        return Ok(());
    };
    match uuid::Uuid::parse_str(namespace) {
        Ok(universe) if universe != universe_id => Err(AgentApiError::invalid_request(format!(
            "{label} names a workflow of another universe"
        ))),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::SessionStatus;

    #[test]
    fn only_an_ended_execution_reports_an_open_session_failure() {
        use api::SessionExecutionFailureView as View;
        for (status, expected) in [
            (WorkflowExecutionStatus::Running, None),
            (WorkflowExecutionStatus::ContinuedAsNew, None),
            (WorkflowExecutionStatus::Paused, None),
            (WorkflowExecutionStatus::Unspecified, None),
            (WorkflowExecutionStatus::Unknown, None),
            (WorkflowExecutionStatus::Failed, Some(View::Failed)),
            (WorkflowExecutionStatus::TimedOut, Some(View::TimedOut)),
            (WorkflowExecutionStatus::Terminated, Some(View::Terminated)),
            // Under a row that is still open these did not end cleanly either.
            (WorkflowExecutionStatus::Completed, Some(View::Terminated)),
            (WorkflowExecutionStatus::Canceled, Some(View::Terminated)),
        ] {
            assert_eq!(
                ExecutionLiveness::from_status(status).open_session_failure(SessionStatus::Idle),
                expected,
                "{status:?}"
            );
        }
        assert_eq!(
            ExecutionLiveness::Missing.open_session_failure(SessionStatus::Idle),
            Some(View::Missing)
        );
    }

    #[test]
    fn a_row_nothing_has_opened_only_fails_on_an_execution_that_ran() {
        use api::SessionExecutionFailureView as View;
        assert_eq!(
            ExecutionLiveness::Missing.open_session_failure(SessionStatus::NotLoaded),
            None
        );
        assert_eq!(
            ExecutionLiveness::Terminal(ExecutionTerminalKind::Failed)
                .open_session_failure(SessionStatus::NotLoaded),
            Some(View::Failed)
        );
    }

    #[test]
    fn a_stopped_execution_is_classified_by_what_the_store_has() {
        use ExecutionTerminalKind::{Completed, Failed, Terminated};
        let session_id = SessionId::new("session_1");
        let no_row = || Err(AgentApiError::not_found("session not found: session_1"));
        for (row, liveness, kind, message) in [
            (
                Ok(CoreAgentStatus::Closed),
                ExecutionLiveness::Terminal(Completed),
                AgentApiErrorKind::Rejected,
                "session is not open: session_1",
            ),
            // A deleted session stays not found, so bots still see it as missing.
            (
                no_row(),
                ExecutionLiveness::Terminal(Completed),
                AgentApiErrorKind::NotFound,
                "session not found: session_1",
            ),
            (
                no_row(),
                ExecutionLiveness::Terminal(Terminated),
                AgentApiErrorKind::NotFound,
                "session not found: session_1",
            ),
            (
                no_row(),
                ExecutionLiveness::Missing,
                AgentApiErrorKind::NotFound,
                "session not found: session_1",
            ),
            (
                Ok(CoreAgentStatus::Open),
                ExecutionLiveness::Missing,
                AgentApiErrorKind::NotFound,
                "agent workflow not found",
            ),
        ] {
            let error = stopped_execution_not_found_error(&session_id, row, liveness);
            assert_eq!(error.kind, kind, "{liveness:?}");
            assert_eq!(error.message, message, "{liveness:?}");
        }
        for (row, liveness) in [
            (no_row(), ExecutionLiveness::Terminal(Failed)),
            (
                Ok(CoreAgentStatus::Open),
                ExecutionLiveness::Terminal(Terminated),
            ),
        ] {
            assert_eq!(
                stopped_execution_not_found_error(&session_id, row, liveness),
                session_bootstrap_failed_error(&session_id, None)
            );
        }
    }

    #[test]
    fn workflow_references_stay_inside_the_callers_universe() {
        let mine = uuid::Uuid::from_u128(1);
        let theirs = uuid::Uuid::from_u128(2);
        let check = |id: &str| require_universe_workflow_reference(mine, "receiver", id);
        check("plugin/approvals-1").expect("plugin namespace");
        check("acorn-review-coordinator").expect("no namespace");
        check(&format!("{mine}/bot-reviewer")).expect("own universe");
        assert_eq!(
            check(&format!("{theirs}/bot-reviewer"))
                .expect_err("other universe")
                .kind,
            AgentApiErrorKind::InvalidRequest
        );
    }

    #[test]
    fn context_update_acknowledges_current_catalog_with_retained_history() {
        let key =
            ContextEntryKey::new(tools::skills::environment::ENVIRONMENT_SKILL_CATALOG_CONTEXT_KEY);
        let mut state = harness::CoreAgentState::new();
        for (id, text) in [(1, "old skills"), (2, "updated skills")] {
            state.context.entries.push(ContextEntry {
                entry_id: harness::ContextEntryId::new(id),
                key: Some(key.clone()),
                kind: ContextEntryKind::Catalog {
                    title: "Environment skills".into(),
                },
                source: harness::ContextEntrySource::ContextEdit,
                content: harness::ContentRef {
                    content_ref: BlobRef::from_bytes(text.as_bytes()),
                    media_type: Some("text/markdown".into()),
                    provider_kind: None,
                },
                preview: Some("Environment skills".into()),
                origin: Some("runtime.environment:test".into()),
                provenance_ref: None,
                token_estimate: None,
                supersedes: (id == 2).then(|| harness::ContextEntryId::new(1)),
            });
        }
        let old = active_entry_input(&state.context.entries[0]);
        let current = active_entry_input(&state.context.entries[1]);

        assert_eq!(
            matching_current_context_entry(&state, &key, &current)
                .expect("the committed update must be acknowledged despite retained history")
                .entry_id,
            harness::ContextEntryId::new(2),
        );
        assert!(
            matching_current_context_entry(&state, &key, &old).is_none(),
            "a superseded version must not acknowledge an update"
        );
        assert!(
            matching_current_context_entry(
                &state,
                &ContextEntryKey::new("runtime.catalog.missing"),
                &current,
            )
            .is_none()
        );
    }
}
