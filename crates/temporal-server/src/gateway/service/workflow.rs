use super::*;

impl GatewayAgentApi {
    /// Universe (tenant) this gateway instance serves, taken from the bound
    /// store. All Temporal addressing composes it into the workflow id.
    pub(super) fn universe_id(&self) -> uuid::Uuid {
        self.store.config().universe_id
    }

    pub(super) fn workflow_id_for(&self, session_id: &SessionId) -> String {
        temporal_workflow::compose_workflow_id(self.universe_id(), session_id)
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

    /// Encodes commands and signals them as one admission batch. Context
    /// upserts carry their key so admission failures can be attributed back
    /// to the entry that caused them.
    pub(super) async fn submit_core_commands(
        &self,
        session_id: &SessionId,
        commands: Vec<CoreAgentCommand>,
    ) -> Result<(), AgentApiError> {
        let mut admissions = Vec::with_capacity(commands.len());
        for command in commands {
            let context_key = match &command {
                CoreAgentCommand::UpsertContext { key, .. }
                | CoreAgentCommand::RemoveContext { key } => Some(key.clone()),
                _ => None,
            };
            let command = engine::CoreAgentCodec
                .encode_command(&command)
                .map_err(|error| AgentApiError::internal(error.to_string()))?;
            admissions.push(AgentAdmission {
                command,
                context_key,
            });
        }
        self.signal_submit_admissions(session_id, admissions).await
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

    pub(super) async fn wait_for_open_session(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionView, AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for agent session to open: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            match self.project_session_by_id(session_id).await {
                Ok(session) if session.config.is_some() => return Ok(session),
                Ok(_) => {}
                Err(error) if is_not_found(&error) => {}
                Err(error) => return Err(error),
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(super) async fn wait_for_config_revision(
        &self,
        session_id: &SessionId,
        target_revision: u64,
        baseline_failures: usize,
    ) -> Result<SessionView, AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for agent session config update: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if status.admission_failures.len() > baseline_failures {
                    if let Some(failure) = status.admission_failures.last() {
                        return Err(map_admission_failure_to_api_error(failure));
                    }
                }
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let session = self.project_session_by_id(session_id).await?;
            if session.config_revision >= target_revision {
                return Ok(session);
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(super) async fn wait_for_tool_revision(
        &self,
        session_id: &SessionId,
        target_revision: u64,
        baseline_failures: usize,
    ) -> Result<SessionView, AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for agent tools update: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if status.admission_failures.len() > baseline_failures {
                    if let Some(failure) = status.admission_failures.last() {
                        return Err(map_admission_failure_to_api_error(failure));
                    }
                }
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            if loaded.state.tooling.revision >= target_revision {
                return self.project_session_by_id(session_id).await;
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    /// Waits for exact context entries to commit; any per-entry admission
    /// failure is escalated to a call-level typed error. Built on the same
    /// wait loop as `context/append`.
    pub(super) async fn wait_for_context_entries_applied(
        &self,
        session_id: &SessionId,
        expected: &[(ContextEntryKey, ContextEntryInput)],
        baseline_failures: usize,
    ) -> Result<u64, AgentApiError> {
        let (context_revision, outcomes) = self
            .wait_for_context_append_outcomes(session_id, expected, baseline_failures)
            .await?;
        for outcome in outcomes.values() {
            if let ContextAppendWaitOutcome::Failed { failure } = outcome {
                return Err(map_admission_failure_to_api_error(failure));
            }
        }
        Ok(context_revision)
    }

    pub(super) async fn wait_for_context_append_outcomes(
        &self,
        session_id: &SessionId,
        expected: &[(ContextEntryKey, ContextEntryInput)],
        baseline_failures: usize,
    ) -> Result<(u64, BTreeMap<ContextEntryKey, ContextAppendWaitOutcome>), AgentApiError> {
        let expected_keys = expected
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<BTreeSet<_>>();
        let started = Instant::now();
        let mut outcomes = BTreeMap::new();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for context entries to apply: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                for failure in status.admission_failures.iter().skip(baseline_failures) {
                    let Some(key) = failure.context_key.as_ref() else {
                        continue;
                    };
                    if expected_keys.contains(key) {
                        outcomes.entry(key.clone()).or_insert_with(|| {
                            ContextAppendWaitOutcome::Failed {
                                failure: failure.clone(),
                            }
                        });
                    }
                }
                if let Some(error) = status.last_error {
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
                if let Some(active) = loaded
                    .state
                    .context
                    .entries
                    .iter()
                    .find(|entry| entry.key.as_ref() == Some(key))
                    .filter(|entry| active_context_entry_matches_input(entry, input))
                {
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
        baseline_failures: usize,
    ) -> Result<(u64, BTreeMap<ContextEntryKey, Option<AgentAdmissionFailure>>), AgentApiError>
    {
        let expected_keys = expected.iter().cloned().collect::<BTreeSet<_>>();
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
                for failure in status.admission_failures.iter().skip(baseline_failures) {
                    let Some(key) = failure.context_key.as_ref() else {
                        continue;
                    };
                    if expected_keys.contains(key) {
                        outcomes
                            .entry(key.clone())
                            .or_insert_with(|| Some(failure.clone()));
                    }
                }
                if let Some(error) = status.last_error {
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
                if status.admission_failures.len() > baseline_failures {
                    if let Some(failure) = status.admission_failures.last() {
                        return Err(map_admission_failure_to_api_error(failure));
                    }
                }
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            if loaded.state.context.revision > baseline_revision
                && !loaded.state.context.pending_compaction
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
                return Err(map_admission_failure_to_api_error(failure));
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
                    .project_run_by_id(session_id, RunId::new(active.run_id), active.status)
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
                    .project_run_by_id(session_id, RunId::new(run.run_id), run.status)
                    .await;
            }
            if let Some(error) = status.last_error {
                return Err(AgentApiError::internal(format!(
                    "agent workflow reported error: {error}"
                )));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub(super) async fn wait_for_run_admitted(
        &self,
        session_id: &SessionId,
        submission_id: &SubmissionId,
        baseline_failures: usize,
    ) -> Result<RunId, AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for agent run admission: {submission_id}"
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
                return Err(map_admission_failure_to_api_error(failure));
            }
            if let Some(active) = status
                .active_run
                .as_ref()
                .filter(|run| run.submission_id.as_ref() == Some(submission_id))
            {
                return Ok(RunId::new(active.run_id));
            }
            if let Some(queued) = status
                .queued_runs
                .iter()
                .find(|run| run.submission_id.as_ref() == Some(submission_id))
            {
                return Ok(RunId::new(queued.run_id));
            }
            if let Some(completed) = status
                .completed_runs
                .iter()
                .rev()
                .find(|run| run.submission_id.as_ref() == Some(submission_id))
            {
                return Ok(RunId::new(completed.run_id));
            }
            if let Some(error) = status.last_error {
                return Err(AgentApiError::internal(format!(
                    "agent workflow reported error: {error}"
                )));
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
            if let Some(status) = self.query_status_optional(session_id).await? {
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
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
            if let Some(status) = self.query_status_optional(session_id).await? {
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            if let Some(completed) = loaded
                .state
                .runs
                .completed
                .iter()
                .find(|run| run.run_id == run_id)
            {
                return self
                    .project_run_by_id(session_id, run_id, completed.status)
                    .await;
            }
            if let Some(active) = loaded
                .state
                .runs
                .active
                .as_ref()
                .filter(|run| run.run_id == run_id && run.status != RunStatus::Active)
            {
                return self
                    .project_run_by_id(session_id, run_id, active.status)
                    .await;
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
                        status.last_error.as_deref(),
                    ));
                }
                Ok(Some(status))
            }
            Err(WorkflowQueryError::NotFound(_)) => Ok(None),
            Err(error) => Err(map_workflow_query_error(error)),
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
        match self
            .workflow_handle(session_id)
            .describe(WorkflowDescribeOptions::default())
            .await
        {
            Ok(description) => {
                if matches!(description.status(), WorkflowExecutionStatus::Running) {
                    // Running but the interaction missed it: keep not-found
                    // semantics; the caller will typically retry/poll.
                    AgentApiError::not_found("agent workflow not found")
                } else if self.session_is_closed(session_id).await.unwrap_or(false) {
                    AgentApiError::rejected(format!("session is not open: {session_id}"))
                } else {
                    session_bootstrap_failed_error(session_id, None)
                }
            }
            // Truly absent: there is no execution for this session id.
            Err(WorkflowInteractionError::NotFound(_)) => match self
                .session_is_closed(session_id)
                .await
            {
                Ok(true) => AgentApiError::rejected(format!("session is not open: {session_id}")),
                _ => AgentApiError::not_found("agent workflow not found"),
            },
            Err(error) => map_workflow_interaction_error(error),
        }
    }

    async fn session_is_closed(&self, session_id: &SessionId) -> Result<bool, AgentApiError> {
        self.load_session_state(session_id)
            .await
            .map(|loaded| loaded.state.lifecycle.status == CoreAgentStatus::Closed)
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
