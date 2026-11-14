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

    /// Signal one command under a fresh correlation token. The wait that follows
    /// matches its outcome by this token alone: a token is new per attempt, so an
    /// earlier attempt's failure and another caller's cannot be taken for this one's.
    pub(super) async fn submit_core_command(
        &self,
        session_id: &SessionId,
        command: CoreAgentCommand,
    ) -> Result<String, AgentApiError> {
        let correlation_token = new_correlation_token();
        self.signal_submit_admissions(
            session_id,
            vec![AgentAdmission {
                command,
                correlation_token: Some(correlation_token.clone()),
            }],
        )
        .await?;
        Ok(correlation_token)
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
            let correlation_token = new_correlation_token();
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

    /// Signal an encoded admission batch to the session workflow. A raw Temporal
    /// `NotFound` is classified, so a stopped execution gets its typed error rather
    /// than the misleading "agent workflow not found".
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
        let mut changes = self.store.changes().subscribe(
            self.universe_id(),
            store_pg::changes::ChangeKind::Session,
            session_id.as_str(),
        );
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::operation_pending(format!(
                    "context entries have not applied yet: {session_id}"
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
            store_pg::changes::wait_for_change(&mut changes, self.poll_interval).await;
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
        let mut changes = self.store.changes().subscribe(
            self.universe_id(),
            store_pg::changes::ChangeKind::Session,
            session_id.as_str(),
        );
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::operation_pending(format!(
                    "context entries have not been removed yet: {session_id}"
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
            store_pg::changes::wait_for_change(&mut changes, self.poll_interval).await;
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
        let mut changes = self.store.changes().subscribe(
            self.universe_id(),
            store_pg::changes::ChangeKind::Session,
            session_id.as_str(),
        );
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::operation_pending(format!(
                    "context entries have not been replaced yet: {session_id}"
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
            store_pg::changes::wait_for_change(&mut changes, self.poll_interval).await;
        }
    }

    /// Waits for the manual compaction this call asked for. A compaction that failed
    /// left the context as it was, so it fails the call too.
    pub(super) async fn wait_for_context_compaction_complete(
        &self,
        session_id: &SessionId,
        baseline_revision: u64,
        correlation_token: &str,
    ) -> Result<(), AgentApiError> {
        let started = Instant::now();
        let mut changes = self.store.changes().subscribe(
            self.universe_id(),
            store_pg::changes::ChangeKind::Session,
            session_id.as_str(),
        );
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::operation_pending(format!(
                    "compaction has not finished yet and can still complete: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                check_command_status(&status, correlation_token)?;
            }
            let loaded = self.load_session_state(session_id).await?;
            let compaction = &loaded.state.context.compaction;
            if let Some(outcome) = compaction
                .last_manual_outcome
                .as_ref()
                .filter(|outcome| outcome.revision > baseline_revision)
                .filter(|_| !compaction.is_pending() && !compaction.is_queued())
            {
                return match outcome.status {
                    harness::ContextCompactionStatus::Succeeded => Ok(()),
                    harness::ContextCompactionStatus::Failed => {
                        Err(self.compaction_failed_error(outcome).await)
                    }
                };
            }
            store_pg::changes::wait_for_change(&mut changes, self.poll_interval).await;
        }
    }

    async fn compaction_failed_error(
        &self,
        outcome: &harness::ManualCompactionOutcome,
    ) -> AgentApiError {
        let reason = match &outcome.failure_ref {
            Some(failure_ref) => self.store.read_text(failure_ref).await.ok(),
            None => None,
        };
        AgentApiError::rejected(reason.map_or_else(
            || "context compaction failed; the context is unchanged".to_owned(),
            |reason| format!("context compaction failed; the context is unchanged: {reason}"),
        ))
    }

    pub(super) async fn wait_for_run_accepted(
        &self,
        session_id: &SessionId,
        submission_id: &SubmissionId,
        correlation_token: &str,
    ) -> Result<RunSummaryView, AgentApiError> {
        let started = Instant::now();
        let mut changes = self.store.changes().subscribe(
            self.universe_id(),
            store_pg::changes::ChangeKind::Session,
            session_id.as_str(),
        );
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(run_start_pending_error(submission_id));
            }
            let Some(status) = self.query_status_optional(session_id).await? else {
                store_pg::changes::wait_for_change(&mut changes, self.poll_interval).await;
                continue;
            };
            check_command_status(&status, correlation_token)?;
            if let Some(run_id) = accepted_run_for_submission(&status, submission_id) {
                return self
                    .project_run_summary_by_id(session_id, RunId::new(run_id))
                    .await;
            }
            store_pg::changes::wait_for_change(&mut changes, self.poll_interval).await;
        }
    }

    /// Waits for this submission's steering batch. Only its own input counts: the same
    /// id with other input is on its way to a rejection under `correlation_token`. If
    /// the run leaves the active slot first, the log since `signalled_after` tells
    /// whether the batch reached the model. One that did not never will, so the caller
    /// is free to deliver it another way.
    pub(super) async fn wait_for_steering_accepted(
        &self,
        session_id: &SessionId,
        run_id: RunId,
        submission_id: &SubmissionId,
        input: &[ContextEntryInput],
        signalled_after: harness::EventSeq,
        correlation_token: &str,
    ) -> Result<harness::SteeringId, AgentApiError> {
        let started = Instant::now();
        let mut changes = self.store.changes().subscribe(
            self.universe_id(),
            store_pg::changes::ChangeKind::Session,
            session_id.as_str(),
        );
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(steering_pending_error(run_id, submission_id));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                check_command_status(&status, correlation_token)?;
            }
            let loaded = self.load_session_state(session_id).await?;
            match loaded.state.runs.active.as_ref() {
                Some(active) if active.run_id == run_id => {
                    if let Some(batch) = active
                        .steering_for_submission(submission_id)
                        .filter(|batch| batch.input == input)
                    {
                        return Ok(batch.steering_id);
                    }
                }
                _ => {
                    let entries = self
                        .read_session_entries_since(&loaded.record, signalled_after)
                        .await?;
                    return materialized_steering(&entries, submission_id).ok_or_else(|| {
                        AgentApiError::rejected(format!(
                            "run ended before steering reached it: {}",
                            api_run_id(run_id)
                        ))
                    });
                }
            }
            store_pg::changes::wait_for_change(&mut changes, self.poll_interval).await;
        }
    }

    async fn read_session_entries_since(
        &self,
        record: &harness::storage::SessionRecord,
        after: harness::EventSeq,
    ) -> Result<Vec<harness::CoreAgentEntry>, AgentApiError> {
        let Some(through) = record.head.as_ref().map(|head| head.seq) else {
            return Ok(Vec::new());
        };
        let codec = harness::CoreAgentCodec;
        let mut after = after;
        let mut entries = Vec::new();
        loop {
            let page = self
                .store
                .read_range(harness::storage::ReadSessionEventRange {
                    session_id: record.session_id.clone(),
                    after,
                    through,
                    limit: MAX_RUN_DETAIL_LIMIT,
                })
                .await
                .map_err(map_session_store_error)?;
            for entry in &page.entries {
                entries.push(decode_stored_entry(&codec, entry)?);
            }
            match page.next_after {
                Some(next_after) if !page.complete => after = next_after,
                _ => return Ok(entries),
            }
        }
    }

    /// Waits for this decision's outcome. `started` is shared by a whole decision
    /// batch, so one held behind a compaction costs the batch one timeout and the
    /// rest report pending straight after they are signalled.
    pub(super) async fn wait_for_approval_decision(
        &self,
        session_id: &SessionId,
        run_id: RunId,
        approval_id: &harness::ApprovalId,
        decision: harness::ApprovalDecision,
        correlation_token: &str,
        started: Instant,
    ) -> Result<ApprovalDecisionOutcome, AgentApiError> {
        let mut changes = self.store.changes().subscribe(
            self.universe_id(),
            store_pg::changes::ChangeKind::Session,
            session_id.as_str(),
        );
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::operation_pending(format!(
                    "approval decision has not been admitted yet and can still apply: {approval_id}"
                )));
            }
            let mut lost_race = false;
            if let Some(status) = self.query_status_optional(session_id).await? {
                if let Some(failure) = admission_failure(&status, correlation_token) {
                    let already_terminal = failure.rejection.as_ref().is_some_and(|rejection| {
                        rejection.kind == harness::CommandRejectionKind::ApprovalAlreadyTerminal
                    });
                    if !already_terminal {
                        return Ok(ApprovalDecisionOutcome::Rejected(
                            failure.api_error().message,
                        ));
                    }
                    lost_race = true;
                }
                if let Some(error) = status.fatal_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            let runs = &loaded.state.runs;
            let record = match runs.active.as_ref().filter(|run| run.run_id == run_id) {
                Some(active) => active.approvals.get(approval_id),
                None if runs.finished.iter().any(|run| run.run_id == run_id)
                    || runs.is_pruned(run_id) =>
                {
                    None
                }
                None => return Err(AgentApiError::not_found(format!("run not found: {run_id}"))),
            };
            let terminal_status = record.map(|record| record.status);
            if lost_race {
                return Ok(ApprovalDecisionOutcome::AlreadyTerminal(
                    approval_already_terminal_kind(terminal_status),
                ));
            }
            // The caller saw this approval pending, so a record that is gone was decided
            // and pruned as the run moved on, or the run ended after accepting it. A
            // rejection of this very command, had there been one, was checked first.
            match terminal_status {
                None => return Ok(ApprovalDecisionOutcome::Decided),
                Some(status) if decision_produced_status(decision, status) => {
                    return Ok(ApprovalDecisionOutcome::Decided);
                }
                Some(status) if status.is_terminal() => {
                    return Ok(ApprovalDecisionOutcome::AlreadyTerminal(
                        approval_already_terminal_kind(Some(status)),
                    ));
                }
                Some(_) => {}
            }
            store_pg::changes::wait_for_change(&mut changes, self.poll_interval).await;
        }
    }

    pub(super) async fn wait_for_closed_session(
        &self,
        session_id: &SessionId,
        correlation_token: &str,
    ) -> Result<(), AgentApiError> {
        let started = Instant::now();
        let mut changes = self.store.changes().subscribe(
            self.universe_id(),
            store_pg::changes::ChangeKind::Session,
            session_id.as_str(),
        );
        loop {
            if started.elapsed() > self.operation_timeout.min(CLOSE_WAIT) {
                return Err(AgentApiError::operation_pending(format!(
                    "session has not closed yet and can still close: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                check_command_status(&status, correlation_token)?;
            }
            let loaded = self.load_session_state(session_id).await?;
            if loaded.state.lifecycle.status == CoreAgentStatus::Closed {
                return Ok(());
            }
            store_pg::changes::wait_for_change(&mut changes, self.poll_interval).await;
        }
    }

    pub(super) async fn wait_for_cancelled_run(
        &self,
        session_id: &SessionId,
        run_id: RunId,
        correlation_token: &str,
    ) -> Result<RunSummaryView, AgentApiError> {
        let started = Instant::now();
        let mut changes = self.store.changes().subscribe(
            self.universe_id(),
            store_pg::changes::ChangeKind::Session,
            session_id.as_str(),
        );
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::operation_pending(format!(
                    "cancellation has not been admitted yet and can still land: {}",
                    api_run_id(run_id)
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                check_command_status(&status, correlation_token)?;
            }
            let loaded = self.load_session_state(session_id).await?;
            if loaded
                .state
                .runs
                .finished
                .iter()
                .any(|run| run.run_id == run_id)
            {
                return self.project_run_summary_by_id(session_id, run_id).await;
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
                return self.project_run_summary_by_id(session_id, run_id).await;
            }
            store_pg::changes::wait_for_change(&mut changes, self.poll_interval).await;
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
        status: api::SessionLifecycleStatus,
    ) -> Option<api::SessionExecutionFailureView> {
        let observed = self.store.load_session(session_id).await.ok().flatten();
        match self.execution_liveness(session_id).await {
            Ok(liveness) => {
                let failure = liveness.open_session_failure(status);
                if let Some(observed) = observed
                    && (failure.is_some() || observed.summary.execution_failure.is_some())
                {
                    use harness::storage::summary::ExecutionFailure as Stored;
                    let stored = failure.as_ref().map(|failure| match failure {
                        api::SessionExecutionFailureView::Failed => Stored::Failed,
                        api::SessionExecutionFailureView::Terminated => Stored::Terminated,
                        api::SessionExecutionFailureView::TimedOut => Stored::TimedOut,
                        api::SessionExecutionFailureView::Missing => Stored::Missing,
                    });
                    if let Err(error) = self
                        .store
                        .record_execution_failure(
                            session_id,
                            Some(observed.revision),
                            stored,
                            now_ms()
                                .map(|time| time.max(0) as u64)
                                .unwrap_or(observed.updated_at_ms),
                        )
                        .await
                    {
                        tracing::warn!(%error, %session_id, "could not record execution observation");
                    }
                }
                failure
            }
            Err(error) => {
                tracing::warn!(target: "temporal_runtime", %session_id, %error, "could not check the session's workflow execution");
                None
            }
        }
    }

    /// Say why a signal or query found no running execution: an open session's
    /// execution stopped, a bootstrap failed, or the session is not there at all.
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

    /// Close whatever the session is doing, for a caller already allowed to.
    pub(super) async fn force_close_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), AgentApiError> {
        // Prefer the live workflow: it cancels active work, appends the close,
        // observes closed+quiescent, and exits itself.
        if matches!(
            self.execution_liveness(session_id).await,
            Ok(ExecutionLiveness::Running)
        ) {
            let closed = match self
                .submit_core_command(session_id, CoreAgentCommand::CloseSession { force: true })
                .await
            {
                Ok(correlation_token) => self
                    .wait_for_closed_session(session_id, &correlation_token)
                    .await
                    .is_ok(),
                Err(_) => false,
            };
            if closed {
                return Ok(());
            }
            // The workflow exists but never converged: it is wedged (e.g. a
            // permanently failing workflow task). Terminate it so the direct
            // append below is the only writer, then reconcile the log.
            let _ = self
                .workflow_handle(session_id)
                .terminate(WorkflowTerminateOptions::default())
                .await;
        }
        // No running workflow (operator terminate, bootstrap failure, or the
        // terminate above): reconcile the session log directly. Session and
        // run status are projections of the log, so this alone recovers the
        // row; the expected-head CAS protects against a concurrent writer.
        self.force_close_session_in_store(session_id).await
    }

    /// Force-close recovery path for sessions with no running workflow:
    /// admits `CloseSession { force: true }` against replayed state and
    /// appends the resulting events (force-cancel active run, drop queued
    /// runs, close) directly to the session store. Status is a projection of
    /// the log, so this alone reconciles the session row; the expected-head
    /// CAS makes a race with any concurrent writer fail here rather than
    /// corrupt the log. No workflow is left to tell anyone, so this also sends
    /// the terminal notifications.
    pub(super) async fn force_close_session_in_store(
        &self,
        session_id: &SessionId,
    ) -> Result<(), AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        match loaded.state.lifecycle.status {
            // Reached with a workflow this close terminated, which took whatever it
            // had not sent yet with it.
            CoreAgentStatus::Closed => {
                self.notify_run_terminals(session_id, &loaded.state).await;
                return Ok(());
            }
            // No log to close: a reserved row is removed, which also frees its
            // delegation slot and lets its tree be deleted.
            CoreAgentStatus::New => {
                if self
                    .store
                    .discard_unopened_session(session_id)
                    .await
                    .map_err(map_session_store_error)?
                {
                    return Ok(());
                }
                return Err(AgentApiError::conflict(format!(
                    "session opened while it was being discarded: {session_id}"
                )));
            }
            CoreAgentStatus::Open => {}
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
            }
            harness::CoreAgentAction::Idle | harness::CoreAgentAction::Closed => {}
            other => {
                return Err(AgentApiError::internal(format!(
                    "force close produced unexpected action: {other:?}"
                )));
            }
        }
        let closed = self.load_session_state(session_id).await?;
        self.notify_run_terminals(session_id, &closed.state).await;
        Ok(())
    }

    /// Signal every notification on the session's retained run records, not only the
    /// runs this close finished: a stopped workflow also took with it any it had
    /// queued for runs that ended earlier. Receivers know a repeat by its emission
    /// id. A receiver that is not running misses it, as the field's contract says,
    /// and reconciles through the run read. That and a failed signal cost nothing
    /// here: the session is closed either way.
    async fn notify_run_terminals(&self, session_id: &SessionId, state: &harness::CoreAgentState) {
        for pending in retained_run_terminals(self.universe_id(), session_id, state) {
            let signalled = self
                .client
                .get_workflow_handle::<AgentSessionWorkflow>(pending.receiver_workflow_id.clone())
                .signal(
                    AgentSessionWorkflow::deliver_emission,
                    temporal_workflow::emission_signal_input(&pending.envelope),
                    WorkflowSignalOptions::default(),
                )
                .await;
            if let Err(error) = signalled
                && !matches!(error, WorkflowInteractionError::NotFound(_))
            {
                tracing::warn!(
                    target: "temporal_runtime",
                    session_id = %session_id,
                    receiver = %pending.receiver_workflow_id,
                    error = %error,
                    "could not send a run's terminal notification after a force close"
                );
            }
        }
    }
}

fn retained_run_terminals(
    universe_id: uuid::Uuid,
    session_id: &SessionId,
    state: &harness::CoreAgentState,
) -> Vec<temporal_workflow::PendingEmission> {
    state
        .runs
        .finished
        .iter()
        .flat_map(|record| {
            temporal_workflow::run_terminal_emissions(
                universe_id,
                session_id,
                record.terminal_seq,
                record,
            )
        })
        .collect()
}

/// Longest a close waits to see its session closed. A bot controller closes from an
/// activity, which has to outlive this wait with room left for a force close to
/// terminate the workflow and write the log. A healthy session answers a close long
/// before this.
const CLOSE_WAIT: Duration = Duration::from_secs(45);
const _: () = assert!(
    CLOSE_WAIT.as_secs() + 10 < temporal_workflow::bots::BOT_ACTIVITY_START_TO_CLOSE.as_secs()
);

fn new_correlation_token() -> String {
    format!("admit_{}", uuid::Uuid::new_v4().simple())
}

/// This command's recorded refusal, if the workflow has made one.
pub(super) fn admission_failure<'a>(
    status: &'a AgentSessionStatus,
    correlation_token: &str,
) -> Option<&'a AgentAdmissionFailure> {
    status
        .admission_failures
        .iter()
        .find(|failure| failure.correlation_token.as_deref() == Some(correlation_token))
}

/// What ends every command wait early: its own refusal, or a workflow that is failing.
pub(super) fn check_command_status(
    status: &AgentSessionStatus,
    correlation_token: &str,
) -> Result<(), AgentApiError> {
    if let Some(failure) = admission_failure(status, correlation_token) {
        return Err(failure.api_error());
    }
    match &status.fatal_error {
        Some(error) => Err(AgentApiError::internal(format!(
            "agent workflow reported error: {error}"
        ))),
        None => Ok(()),
    }
}

/// The run a submission landed as, once nothing the workflow holds could still refuse
/// that submission. A resubmission with other input is only refused when it is
/// processed, so a match with it still pending proves nothing. Commands other than
/// this one do not delay the answer. A run accepted behind an active run counts: it
/// comes back `queued`, and callers follow events for its start.
fn accepted_run_for_submission(
    status: &AgentSessionStatus,
    submission_id: &SubmissionId,
) -> Option<u64> {
    if status.pending_submission_ids.contains(submission_id) {
        return None;
    }
    status
        .active_run
        .iter()
        .filter(|run| run.submission_id.as_ref() == Some(submission_id))
        .map(|run| run.run_id)
        .chain(
            status
                .finished_runs
                .iter()
                .rev()
                .filter(|run| run.submission_id.as_ref() == Some(submission_id))
                .map(|run| run.run_id),
        )
        .chain(
            status
                .queued_runs
                .iter()
                .filter(|run| run.submission_id.as_ref() == Some(submission_id))
                .map(|run| run.run_id),
        )
        .next()
}

/// The expired wait of a run start. The signal is still queued, and the run lands when
/// the session gets to it, so a retry has to name the same submission.
fn run_start_pending_error(submission_id: &SubmissionId) -> AgentApiError {
    pending_submission_error(
        format!(
            "the session has not accepted this run yet and can still start it; retry with submission id {submission_id}"
        ),
        submission_id,
    )
}

/// The expired wait of a steering. As with a run start, the signal is still queued.
fn steering_pending_error(run_id: RunId, submission_id: &SubmissionId) -> AgentApiError {
    pending_submission_error(
        format!(
            "steering has not been admitted yet and can still land on {}; retry with submission id {submission_id}",
            api_run_id(run_id)
        ),
        submission_id,
    )
}

fn pending_submission_error(message: String, submission_id: &SubmissionId) -> AgentApiError {
    let mut error = AgentApiError::operation_pending(message);
    error.submission_id = Some(submission_id.as_str().to_owned());
    error
}

/// The batch this submission named, if it reached the run's context. A batch accepted
/// and never written down was dropped by the run's cancellation.
fn materialized_steering(
    entries: &[harness::CoreAgentEntry],
    submission_id: &SubmissionId,
) -> Option<harness::SteeringId> {
    let steering_id = entries.iter().find_map(|entry| match &entry.event {
        harness::CoreAgentEvent::Run(harness::RunEvent::SteeringAccepted {
            steering_id,
            submission_id: accepted,
            ..
        }) if accepted == submission_id => Some(*steering_id),
        _ => None,
    })?;
    entries
        .iter()
        .any(|entry| match &entry.event {
            harness::CoreAgentEvent::Context(harness::ContextEvent::EntriesApplied {
                entries,
                ..
            }) => entries.iter().any(|applied| {
                matches!(
                    applied.source,
                    harness::ContextEntrySource::Steering { steering_id: id, .. } if id == steering_id
                )
            }),
            _ => false,
        })
        .then_some(steering_id)
}

/// Where an approval decision ended up for the caller that sent it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ApprovalDecisionOutcome {
    Decided,
    /// Another decision or a cancellation got there first.
    AlreadyTerminal(ApprovalDecisionFailureKind),
    Rejected(String),
}

fn decision_produced_status(
    decision: harness::ApprovalDecision,
    status: harness::ApprovalStatus,
) -> bool {
    matches!(
        (decision, status),
        (
            harness::ApprovalDecision::Approved,
            harness::ApprovalStatus::Approved
        ) | (
            harness::ApprovalDecision::Rejected,
            harness::ApprovalStatus::Rejected
        )
    )
}

/// `None` is a record already pruned, which only a decision leaves behind.
pub(super) fn approval_already_terminal_kind(
    status: Option<harness::ApprovalStatus>,
) -> ApprovalDecisionFailureKind {
    match status {
        Some(harness::ApprovalStatus::Cancelled) => ApprovalDecisionFailureKind::Cancelled,
        _ => ApprovalDecisionFailureKind::AlreadyDecided,
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
    /// How an execution that is no longer running ended, in words for a person.
    pub(crate) fn stopped_description(self) -> Option<String> {
        let ended = match self {
            Self::Running => return None,
            Self::Terminal(ExecutionTerminalKind::Completed) => "completed",
            Self::Terminal(ExecutionTerminalKind::Failed) => "failed",
            Self::Terminal(ExecutionTerminalKind::Canceled) => "was canceled",
            Self::Terminal(ExecutionTerminalKind::Terminated) => "was terminated",
            Self::Terminal(ExecutionTerminalKind::TimedOut) => "timed out",
            Self::Missing => "no longer exists",
        };
        Some(format!("its workflow execution {ended}"))
    }

    pub(crate) fn from_status(status: WorkflowExecutionStatus) -> Self {
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
        status: api::SessionLifecycleStatus,
    ) -> Option<api::SessionExecutionFailureView> {
        use api::SessionExecutionFailureView as View;
        match self {
            Self::Running => None,
            // A row nothing has opened is not dead for having no execution: a
            // sub-agent's row is written before its workflow starts, and a
            // retried start still opens one.
            Self::Missing if status == api::SessionLifecycleStatus::New => None,
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
        // An open row has a log, however long ago its bootstrap finished.
        (Ok(CoreAgentStatus::Open), _) => session_execution_stopped_error(session_id),
        // The store could not say, which is no statement about the session.
        (Err(error), _) if !is_not_found(&error) => error,
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

pub(super) fn session_execution_stopped_error(session_id: &SessionId) -> AgentApiError {
    AgentApiError::session_execution_stopped(format!(
        "the workflow execution of session {session_id} has stopped; its history is intact. \
         session/restart brings it back"
    ))
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
    use api::SessionLifecycleStatus;

    fn run_record(
        run: u64,
        status: harness::RunStatus,
        intents: &[(&str, &str)],
    ) -> harness::RunRecord {
        harness::RunRecord {
            run_id: harness::RunId::new(run),
            status,
            submission_id: None,
            submission_digest: 0,
            source: harness::RunSource::Input { input: Vec::new() },
            first_seq: harness::EventSeq::new(run),
            terminal_seq: harness::EventSeq::new(run + 1),
            accepted_at_ms: 1,
            started_at_ms: Some(1),
            finished_at_ms: 1,
            usage: None,
            output: None,
            failure: None,
            notify_on_terminal: intents
                .iter()
                .map(|(holder, token)| harness::RunTerminalNotifyIntent {
                    holder_workflow_id: (*holder).to_owned(),
                    token: (*token).to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_force_close_notifies_every_intent_on_the_retained_runs() {
        let mut state = harness::CoreAgentState::new();
        state.runs.finished.extend([
            run_record(1, harness::RunStatus::Completed, &[("u/bot", "token_1")]),
            run_record(2, harness::RunStatus::Completed, &[]),
            run_record(
                3,
                harness::RunStatus::Cancelled,
                &[("u/bot", "token_3"), ("u/parent", "token_3b")],
            ),
        ]);
        let universe = uuid::Uuid::from_u128(7);
        let session = SessionId::new("bot:v1:triage");

        let sent = retained_run_terminals(universe, &session, &state);

        let addressed: Vec<(&str, u64)> = sent
            .iter()
            .map(|pending| {
                let harness::EmissionBody::RunTerminal { run_id, .. } = &pending.envelope.body
                else {
                    panic!("only run terminals: {pending:?}");
                };
                (pending.receiver_workflow_id.as_str(), run_id.as_u64())
            })
            .collect();
        assert_eq!(addressed, vec![("u/bot", 1), ("u/bot", 3), ("u/parent", 3)]);
        assert!(matches!(
            &sent[1].envelope.body,
            harness::EmissionBody::RunTerminal {
                status: harness::RunStatus::Cancelled,
                token,
                ..
            } if token == "token_3"
        ));
        // Repeating the close repeats the id, which is how a receiver drops it.
        let again = retained_run_terminals(universe, &session, &state);
        assert_eq!(again[1].envelope.emission_id, sent[1].envelope.emission_id);
    }

    #[test]
    fn a_session_with_no_notifications_signals_nobody() {
        let state = harness::CoreAgentState::new();
        let session = SessionId::new("session_1");
        assert!(retained_run_terminals(uuid::Uuid::nil(), &session, &state).is_empty());
    }

    fn empty_status() -> AgentSessionStatus {
        AgentSessionStatus {
            session_id: "session_1".to_owned(),
            initialized: true,
            ready: true,
            setup_error: None,
            pending_admissions: 0,
            pending_submission_ids: Vec::new(),
            pending_tool_batch_unparks: 0,
            active_waits: 0,
            pending_emissions: 0,
            active_run: None,
            queued_runs: Vec::new(),
            finished_runs: Vec::new(),
            admission_failures: Vec::new(),
            fatal_error: None,
            last_warning: None,
            bootstrap_failed: false,
        }
    }

    fn refused(
        token: &str,
        submission_id: &str,
        kind: harness::CommandRejectionKind,
    ) -> AgentAdmissionFailure {
        let rejection = harness::CommandRejection::new(kind, "refused");
        AgentAdmissionFailure {
            preparation_error: None,
            submission_id: Some(SubmissionId::new(submission_id)),
            correlation_token: Some(token.to_owned()),
            kind: AgentAdmissionFailureKind::RejectedCommand,
            message: rejection.to_string(),
            rejection: Some(rejection),
        }
    }

    #[test]
    fn a_waiter_only_hears_the_refusal_of_its_own_token() {
        use harness::CommandRejectionKind::{ActiveWork, Refused};
        let mut status = empty_status();
        // Someone else's refusal arrives while this command waits, and an earlier
        // attempt under the same submission id was refused too.
        status.admission_failures = vec![
            refused("admit_earlier_attempt", "submission", Refused),
            refused("admit_other_caller", "other", ActiveWork),
        ];
        assert_eq!(check_command_status(&status, "admit_mine"), Ok(()));

        status
            .admission_failures
            .insert(0, refused("admit_mine", "submission", ActiveWork));
        assert_eq!(
            check_command_status(&status, "admit_mine")
                .expect_err("its own refusal")
                .kind,
            AgentApiErrorKind::Rejected
        );

        let mut status = empty_status();
        status.fatal_error = Some("boom".to_owned());
        assert_eq!(
            check_command_status(&status, "admit_mine")
                .expect_err("a failing workflow")
                .kind,
            AgentApiErrorKind::Internal
        );
    }

    #[test]
    fn a_run_is_answered_by_its_own_submission_and_not_by_the_queue_being_empty() {
        let submission = SubmissionId::new("submission");
        let mut status = empty_status();
        assert_eq!(accepted_run_for_submission(&status, &submission), None);

        status.queued_runs = vec![temporal_workflow::AgentQueuedRunSummary {
            run_id: 7,
            submission_id: Some(submission.clone()),
            input: Vec::new(),
        }];
        // Unrelated commands held behind a slow generation do not delay it.
        status.pending_admissions = 3;
        status.pending_submission_ids = vec![SubmissionId::new("someone_else")];
        assert_eq!(accepted_run_for_submission(&status, &submission), Some(7));

        // This resubmission is still queued and may yet be refused for other input.
        status.pending_submission_ids.push(submission.clone());
        assert_eq!(accepted_run_for_submission(&status, &submission), None);
    }

    #[test]
    fn an_expired_submission_hands_back_the_id_to_retry_with() {
        let submission = SubmissionId::new("submission");
        for error in [
            run_start_pending_error(&submission),
            steering_pending_error(RunId::new(1), &submission),
        ] {
            assert_eq!(error.kind, AgentApiErrorKind::OperationPending);
            assert_eq!(error.submission_id.as_deref(), Some("submission"));
        }
    }

    #[test]
    fn a_decision_is_told_apart_from_the_one_that_beat_it() {
        use harness::{ApprovalDecision as Decision, ApprovalStatus as Status};
        assert!(decision_produced_status(
            Decision::Approved,
            Status::Approved
        ));
        assert!(decision_produced_status(
            Decision::Rejected,
            Status::Rejected
        ));
        assert!(!decision_produced_status(
            Decision::Rejected,
            Status::Approved
        ));
        assert!(!decision_produced_status(
            Decision::Approved,
            Status::Cancelled
        ));
        assert!(!decision_produced_status(
            Decision::Approved,
            Status::Pending
        ));
        assert_eq!(
            approval_already_terminal_kind(Some(Status::Cancelled)),
            ApprovalDecisionFailureKind::Cancelled
        );
        assert_eq!(
            approval_already_terminal_kind(Some(Status::Approved)),
            ApprovalDecisionFailureKind::AlreadyDecided
        );
        assert_eq!(
            approval_already_terminal_kind(None),
            ApprovalDecisionFailureKind::AlreadyDecided
        );
    }

    #[test]
    fn steering_counts_as_landed_only_once_it_reached_context() {
        let submission = SubmissionId::new("delivery");
        let run_id = RunId::new(1);
        let steering_id = harness::SteeringId::new(4);
        let accepted = |seq, submission_id: &SubmissionId| harness::CoreAgentEntry {
            position: harness::session::SessionPosition {
                seq: harness::EventSeq::new(seq),
            },
            observed_at_ms: 1,
            joins: Default::default(),
            event: harness::CoreAgentEvent::Run(harness::RunEvent::SteeringAccepted {
                run_id,
                steering_id,
                submission_id: submission_id.clone(),
                input: Vec::new(),
                requested_by: None,
            }),
        };
        let written = harness::CoreAgentEntry {
            event: harness::CoreAgentEvent::Context(harness::ContextEvent::EntriesApplied {
                base_revision: 0,
                entries: vec![ContextEntry {
                    entry_id: harness::ContextEntryId::new(1),
                    key: None,
                    kind: ContextEntryKind::Message {
                        role: harness::ContextMessageRole::User,
                    },
                    source: harness::ContextEntrySource::Steering {
                        run_id,
                        steering_id,
                        input_index: 0,
                    },
                    content: harness::ContentRef {
                        content_ref: BlobRef::from_bytes(b"steer"),
                        media_type: None,
                        provider_kind: None,
                    },
                    preview: None,
                    origin: None,
                    provenance_ref: None,
                    token_estimate: None,
                    supersedes: None,
                }],
            }),
            ..accepted(2, &submission)
        };
        assert_eq!(materialized_steering(&[], &submission), None);
        assert_eq!(
            materialized_steering(&[accepted(1, &submission)], &submission),
            None,
            "a cancelled run drops what it accepted"
        );
        assert_eq!(
            materialized_steering(&[accepted(1, &submission), written.clone()], &submission),
            Some(steering_id)
        );
        assert_eq!(
            materialized_steering(
                &[accepted(1, &SubmissionId::new("another")), written],
                &submission
            ),
            None
        );
    }

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
                ExecutionLiveness::from_status(status)
                    .open_session_failure(SessionLifecycleStatus::Open),
                expected,
                "{status:?}"
            );
        }
        assert_eq!(
            ExecutionLiveness::Missing.open_session_failure(SessionLifecycleStatus::Open),
            Some(View::Missing)
        );
    }

    #[test]
    fn a_row_nothing_has_opened_only_fails_on_an_execution_that_ran() {
        use api::SessionExecutionFailureView as View;
        assert_eq!(
            ExecutionLiveness::Missing.open_session_failure(SessionLifecycleStatus::New),
            None
        );
        assert_eq!(
            ExecutionLiveness::Terminal(ExecutionTerminalKind::Failed)
                .open_session_failure(SessionLifecycleStatus::New),
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
            // Nothing was opened, so there is nothing that stopped.
            (
                Ok(CoreAgentStatus::New),
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
                Ok(CoreAgentStatus::New),
                ExecutionLiveness::Terminal(Failed),
            ),
        ] {
            assert_eq!(
                stopped_execution_not_found_error(&session_id, row, liveness),
                session_bootstrap_failed_error(&session_id, None)
            );
        }
        // A store that cannot answer is reported as itself, whatever the execution did.
        for liveness in [
            ExecutionLiveness::Terminal(Failed),
            ExecutionLiveness::Terminal(Terminated),
            ExecutionLiveness::Missing,
        ] {
            let error = stopped_execution_not_found_error(
                &session_id,
                Err(AgentApiError::internal("store down")),
                liveness,
            );
            assert_eq!(error.kind, AgentApiErrorKind::Internal, "{liveness:?}");
        }
        // However it stopped, an open session is not a bootstrap failure.
        for liveness in [
            ExecutionLiveness::Terminal(Failed),
            ExecutionLiveness::Terminal(Terminated),
            ExecutionLiveness::Terminal(Completed),
            ExecutionLiveness::Missing,
        ] {
            assert_eq!(
                stopped_execution_not_found_error(&session_id, Ok(CoreAgentStatus::Open), liveness),
                session_execution_stopped_error(&session_id)
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
