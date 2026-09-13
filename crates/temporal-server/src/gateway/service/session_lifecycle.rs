//! Session creation, recovery, and preparation at the gateway boundary.
use super::*;
use engine::AdmittedManagedSessionWorkflowTools;

// Only the effects needed to recover and await session creation. Keeping this
// boundary small lets the same recovery logic run against scripted offline I/O.
#[async_trait]
trait SessionLifecycleIo: Sync {
    async fn load(&self, session_id: &SessionId) -> Result<LoadedSession, AgentApiError>;
    async fn is_running(&self, session_id: &SessionId) -> Result<bool, AgentApiError>;
    async fn retry_setup(&self, session_id: &SessionId) -> Result<(), AgentApiError>;
    async fn status(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<AgentSessionStatus>, AgentApiError>;
}

#[async_trait]
impl SessionLifecycleIo for GatewayAgentApi {
    async fn load(&self, session_id: &SessionId) -> Result<LoadedSession, AgentApiError> {
        self.load_session_state(session_id).await
    }

    async fn is_running(&self, session_id: &SessionId) -> Result<bool, AgentApiError> {
        match self
            .workflow_handle(session_id)
            .describe(WorkflowDescribeOptions::default())
            .await
        {
            Ok(description) => Ok(description.status() == WorkflowExecutionStatus::Running),
            Err(WorkflowInteractionError::NotFound(_)) => Ok(false),
            Err(error) => Err(map_workflow_interaction_error(error)),
        }
    }

    async fn retry_setup(&self, session_id: &SessionId) -> Result<(), AgentApiError> {
        self.workflow_handle(session_id)
            .signal(
                AgentSessionWorkflow::retry_setup,
                (),
                WorkflowSignalOptions::default(),
            )
            .await
            .map_err(map_workflow_interaction_error)
    }

    async fn status(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<AgentSessionStatus>, AgentApiError> {
        self.query_status_optional(session_id).await
    }
}

struct SessionLifecycle<'a, T> {
    io: &'a T,
    operation_timeout: Duration,
    poll_interval: Duration,
}

impl<T: SessionLifecycleIo> SessionLifecycle<'_, T> {
    async fn recover_existing(
        &self,
        session_id: &SessionId,
        admitted: Option<&AdmittedManagedSessionWorkflowTools>,
    ) -> Result<Option<LoadedSession>, AgentApiError> {
        match self.io.load(session_id).await {
            Ok(loaded)
                if matches!(
                    loaded.state.lifecycle.status,
                    CoreAgentStatus::Open | CoreAgentStatus::Closed
                ) =>
            {
                return self
                    .resume(session_id, Some(loaded), admitted)
                    .await
                    .map(Some);
            }
            Ok(_) => {}
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
        // Temporal can accept creation before the session row exists.
        if self.io.is_running(session_id).await? {
            return self.resume(session_id, None, admitted).await.map(Some);
        }
        Ok(None)
    }

    async fn recover_conflict(
        &self,
        session_id: &SessionId,
        admitted: Option<&AdmittedManagedSessionWorkflowTools>,
    ) -> Result<LoadedSession, AgentApiError> {
        // Preserve the start-conflict boundary: a missing row is still an
        // error here, rather than silently changing the recovery policy.
        let loaded = self.io.load(session_id).await?;
        self.resume(session_id, Some(loaded), admitted).await
    }

    async fn resume(
        &self,
        session_id: &SessionId,
        known: Option<LoadedSession>,
        admitted: Option<&AdmittedManagedSessionWorkflowTools>,
    ) -> Result<LoadedSession, AgentApiError> {
        let validate_after_wait = known.is_none();
        if let Some(loaded) = known {
            validate_managed_session_retry(&loaded.state, admitted)?;
            if loaded.state.lifecycle.status == CoreAgentStatus::Closed {
                return Ok(loaded);
            }
        }
        self.io.retry_setup(session_id).await?;
        let loaded = self.wait_for_open_session(session_id).await?;
        if validate_after_wait {
            validate_managed_session_retry(&loaded.state, admitted)?;
        }
        Ok(loaded)
    }

    async fn wait_for_open_session(
        &self,
        session_id: &SessionId,
    ) -> Result<LoadedSession, AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for agent session to open: {session_id}"
                )));
            }
            if let Some(status) = self.io.status(session_id).await? {
                if let Some(error) = status.setup_error {
                    return Err(error);
                }
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(error));
                }
                if status.ready {
                    return self.io.load(session_id).await;
                }
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

impl GatewayAgentApi {
    pub async fn open_or_start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        // `start_session` is idempotent on client-supplied session ids; this
        // wrapper remains for callers predating that behavior.
        self.start_session(params).await
    }

    fn allocate_session_id(&self) -> SessionId {
        SessionId::new(format!("session_{}", uuid::Uuid::new_v4().simple()))
    }

    #[allow(clippy::too_many_arguments)]
    fn workflow_args(
        &self,
        session_id: SessionId,
        display_name: Option<String>,
        metadata: BTreeMap<String, String>,
        delete_after_close_ms: Option<u64>,
        session_config: SessionConfig,
        workflow_tools: Option<ManagedSessionWorkflowTools>,
        close_on_terminal: bool,
        auto_reject_approvals: bool,
    ) -> AgentSessionArgs {
        AgentSessionArgs {
            setup: None,
            universe_id: self.universe_id(),
            session_id,
            display_name,
            metadata,
            delete_after_close_ms,
            session_config,
            workflow_tools,
            legacy_max_steps_per_input: None,
            continue_as_new_history_threshold: self.continue_as_new_history_threshold,
            close_on_terminal,
            auto_reject_approvals,
            continuation_state: None,
        }
    }

    /// Sub-agent child creation: the child's store row already
    /// exists with its origin (the execution's reservation); this opens its
    /// workflow with the pinned profile applied. The execution closes the
    /// child, so `close_on_terminal` stays off.
    pub(crate) async fn start_session_for_subagent(
        &self,
        session_id: &SessionId,
        profile: ProfileSource,
    ) -> Result<(), AgentApiError> {
        self.start_session_internal(
            SessionStartParams {
                metadata: Default::default(),
                session_id: Some(session_id.as_str().to_owned()),
                display_name: None,
                config: None,
                profile: Some(profile),
                environment: None,
                // Delegated children inherit their retention root and never
                // apply a profile's root-session default.
                delete_after_close_ms: Some(None),
            },
            false,
            true,
            None,
        )
        .await?;
        Ok(())
    }

    /// Trusted in-process workflow-plugin entry point. The main API exposes
    /// the same immutable target and completion vocabulary through wire DTOs.
    pub async fn start_managed_session_for_workflow_with_profile(
        &self,
        session_id: &SessionId,
        close_on_terminal: bool,
        profile: Option<ProfileSource>,
        workflow_tools: ManagedSessionWorkflowTools,
    ) -> Result<(), AgentApiError> {
        self.start_session_internal(
            SessionStartParams {
                metadata: Default::default(),
                session_id: Some(session_id.as_str().to_owned()),
                display_name: None,
                config: None,
                profile,
                environment: None,
                delete_after_close_ms: None,
            },
            close_on_terminal,
            false,
            Some(workflow_tools),
        )
        .await?;
        Ok(())
    }

    pub(super) async fn start_session_internal(
        &self,
        params: SessionStartParams,
        close_on_terminal: bool,
        auto_reject_approvals: bool,
        trusted_workflow_tools: Option<ManagedSessionWorkflowTools>,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        let SessionStartParams {
            session_id,
            display_name,
            metadata,
            config,
            profile,
            environment,
            delete_after_close_ms,
        } = params;
        validate_caller_metadata(&metadata)?;
        let workflow_tools = trusted_workflow_tools;
        let client_supplied_id = session_id.is_some();
        let session_id = match session_id {
            Some(session_id) => {
                // System workflow ids share the `{universe}/…` namespace
                // with sessions; their segments are reserved.
                if let Some(prefix) = ::bots::ids::RESERVED_SESSION_ID_PREFIXES
                    .iter()
                    .find(|prefix| session_id.starts_with(*prefix))
                {
                    return Err(AgentApiError::invalid_request(format!(
                        "session id prefix `{prefix}` is reserved for system workflows"
                    )));
                }
                SessionId::try_new(session_id).map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid session id: {error}"))
                })?
            }
            None => self.allocate_session_id(),
        };
        let admitted = workflow_tools
            .as_ref()
            .map(|declaration| {
                declaration.admit(self.universe_id()).map_err(|error| {
                    AgentApiError::invalid_request(format!(
                        "invalid managed-session workflow-tool declaration: {error}"
                    ))
                })
            })
            .transpose()?;
        let lifecycle = SessionLifecycle {
            io: self,
            operation_timeout: self.operation_timeout,
            poll_interval: self.poll_interval,
        };
        // Recover the original intent before resolving a mutable named profile.
        if client_supplied_id
            && let Some(loaded) = lifecycle
                .recover_existing(&session_id, admitted.as_ref())
                .await?
        {
            return Ok(self.session_start_response(&loaded));
        }
        let resolved_profile = match profile {
            Some(source) => Some(self.resolve_profile_source(source).await?),
            None => None,
        };
        let effective_metadata = profiles::merge_profile_start_metadata(
            resolved_profile.as_ref().map(|profile| &profile.metadata),
            metadata,
        );
        validate_caller_metadata(&effective_metadata)?;
        let effective_delete_after_close_ms = profiles::merge_profile_start_retention(
            resolved_profile
                .as_ref()
                .and_then(|profile| profile.retention.as_ref())
                .map(|retention| retention.delete_after_close_ms),
            delete_after_close_ms,
        );
        validate_delete_after_close_ms(effective_delete_after_close_ms)?;
        let start_config = self.merge_profile_start_config(
            resolved_profile
                .as_ref()
                .and_then(|profile| profile.config.clone()),
            config,
        );
        let session_config = self.session_config_for_start(start_config).await?;
        // The creation-time environment: an explicit override must be an
        // attachment of the effective configuration; otherwise the default
        // attachment, if any, is activated at setup.
        let setup_environment = match environment {
            Some(SessionEnvironmentOverride::None {}) => None,
            Some(SessionEnvironmentOverride::Existing { environment_id }) => {
                let environment_id = engine::EnvironmentId::try_new(environment_id)
                    .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
                let attached = session_config
                    .features
                    .environments
                    .as_ref()
                    .is_some_and(|environments| environments.is_attached(environment_id.as_str()));
                if !attached {
                    return Err(AgentApiError::invalid_request(format!(
                        "environment {environment_id} is not attached in the session configuration"
                    )));
                }
                Some(environment_id)
            }
            None => profiles::default_environment_id(&session_config.features)?,
        };

        if let Some(admitted) = admitted.as_ref() {
            self.validate_managed_session_materialization(&session_config, admitted)
                .await?;
        }
        let mut args = self.workflow_args(
            session_id.clone(),
            display_name,
            effective_metadata,
            effective_delete_after_close_ms,
            session_config,
            workflow_tools,
            close_on_terminal,
            auto_reject_approvals,
        );
        args.setup = match resolved_profile.as_ref() {
            Some(profile) => {
                let mut intent = self.profile_intent(profile, false)?;
                intent.environment = setup_environment;
                Some(intent)
            }
            None => setup_environment.map(|environment| temporal_workflow::SessionProfileIntent {
                config: None,
                instructions: None,
                environment: Some(environment),
            }),
        };
        self.refresh_input_blob_grace(&args).await?;
        let started = self
            .client
            .start_workflow(
                AgentSessionWorkflow::run,
                args,
                WorkflowStartOptions::new(
                    self.task_queue.clone(),
                    self.workflow_id_for(&session_id),
                )
                .build(),
            )
            .await
            .map_err(map_workflow_start_error);
        match started {
            Ok(_) => {}
            Err(error)
                if matches!(error.kind, AgentApiErrorKind::Conflict) && client_supplied_id =>
            {
                let loaded = lifecycle
                    .recover_conflict(&session_id, admitted.as_ref())
                    .await?;
                return Ok(self.session_start_response(&loaded));
            }
            Err(error) => return Err(error),
        }
        let loaded = lifecycle.wait_for_open_session(&session_id).await?;
        validate_managed_session_retry(&loaded.state, admitted.as_ref())?;
        Ok(self.session_start_response(&loaded))
    }

    fn session_start_response(
        &self,
        loaded: &LoadedSession,
    ) -> AgentApiOutcome<SessionStartResponse> {
        AgentApiOutcome::new(SessionStartResponse {
            session: self.session_mutation_view(loaded),
        })
    }

    async fn validate_managed_session_materialization(
        &self,
        session_config: &SessionConfig,
        admitted: &AdmittedManagedSessionWorkflowTools,
    ) -> Result<(), AgentApiError> {
        for binding in &admitted.bindings {
            validate_workflow_tool_definition_documents(self.store.as_ref(), &binding.definition)
                .await
                .map_err(|error| {
                    AgentApiError::invalid_request(format!(
                        "invalid workflow tool {} documents: {error}",
                        binding.definition.tool_id
                    ))
                })?;
            if let WorkflowToolCompletion::Joined {
                reply_schema_ref: Some(reply_schema_ref),
                ..
            }
            | WorkflowToolCompletion::Promises {
                reply_schema_ref: Some(reply_schema_ref),
                ..
            } = &binding.completion
            {
                validate_workflow_tool_reply_schema(self.store.as_ref(), reply_schema_ref)
                    .await
                    .map_err(|error| {
                        AgentApiError::invalid_request(format!(
                            "invalid workflow tool {} reply schema: {error}",
                            binding.definition.tool_id
                        ))
                    })?;
            }
            if let WorkflowToolTarget::Start { start } = &binding.target {
                self.validate_workflow_tool_start_recipe(&binding.definition.tool_id, start)
                    .await?;
            }
        }

        let materialized_bindings = admitted
            .bindings
            .iter()
            .filter(|binding| !is_core_environment_job_binding(binding))
            .collect::<Vec<_>>();

        let mut config = Self::session_toolset_config(session_config, false, false);
        enable_concurrency_for_workflow_tools(&mut config, materialized_bindings.iter().copied());
        let mut toolset = register_toolset(&config).map_err(|error| {
            AgentApiError::invalid_request(format!("build session tools: {error}"))
        })?;
        register_workflow_tools(&mut toolset, materialized_bindings.iter().copied()).map_err(
            |error| {
                AgentApiError::invalid_request(format!("materialize workflow tool tools: {error}"))
            },
        )?;
        let desired_mcp = self.desired_mcp_tools(&session_config.features).await?;
        if let Some(colliding) = materialized_bindings
            .iter()
            .copied()
            .map(|binding| &binding.definition.tool.name)
            .find(|tool_name| desired_mcp.contains_key(*tool_name))
        {
            return Err(AgentApiError::invalid_request(format!(
                "workflow tool tool name {colliding} collides with a remote MCP tool"
            )));
        }
        Ok(())
    }

    async fn validate_workflow_tool_start_recipe(
        &self,
        tool_id: &WorkflowToolId,
        start: &WorkflowStartRef,
    ) -> Result<(), AgentApiError> {
        let recipe_bytes = self
            .store
            .read_bytes(&start.recipe_ref)
            .await
            .map_err(|error| {
                AgentApiError::invalid_request(format!(
                    "invalid workflow tool {tool_id} start recipe: {error}"
                ))
            })?;
        let observed = temporal_workflow::workflow_tool_recipe_fingerprint(&recipe_bytes);
        if observed != start.recipe_fingerprint {
            return Err(AgentApiError::invalid_request(format!(
                "invalid workflow tool {tool_id} start recipe fingerprint: admitted {} observed {observed}",
                start.recipe_fingerprint
            )));
        }
        if start.recipe_format != temporal_workflow::WORKFLOW_TOOL_RECIPE_FORMAT_V1 {
            return Err(AgentApiError::invalid_request(format!(
                "invalid workflow tool {tool_id} start recipe format {}",
                start.recipe_format
            )));
        }
        let recipe: temporal_workflow::WorkflowToolRecipeV1 = serde_json::from_slice(&recipe_bytes)
            .map_err(|error| {
                AgentApiError::invalid_request(format!(
                    "invalid workflow tool {tool_id} start recipe v1: {error}"
                ))
            })?;
        if recipe.workflow_type.is_empty() || recipe.task_queue.is_empty() {
            return Err(AgentApiError::invalid_request(format!(
                "invalid workflow tool {tool_id} start recipe v1: workflowType and taskQueue are required"
            )));
        }
        Ok(())
    }

    pub(super) async fn prepare_session_operation(
        &self,
        session_id: &SessionId,
        operation: temporal_workflow::SessionOperation,
    ) -> Result<api::ProfileApplySummary, AgentApiError> {
        let request = temporal_workflow::SessionOperationRequest {
            operation_id: format!("prepare_{}", uuid::Uuid::new_v4().simple()),
            submitted_at_ms: u64::try_from(now_ms()?)
                .map_err(|error| AgentApiError::internal(error.to_string()))?,
            operation,
        };
        let receipt = request
            .receipt()
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        self.refresh_input_blob_grace(&request).await?;
        self.workflow_handle(session_id)
            .signal(
                AgentSessionWorkflow::prepare_session,
                request.clone(),
                WorkflowSignalOptions::default(),
            )
            .await
            .map_err(map_workflow_interaction_error)?;
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for session preparation: {}",
                    request.operation_id
                )));
            }
            let outcome = self
                .workflow_handle(session_id)
                .query(
                    AgentSessionWorkflow::operation_outcome,
                    receipt.clone(),
                    WorkflowQueryOptions::default(),
                )
                .await
                .map_err(map_workflow_query_error)?
                .outcome?;
            if let Some(outcome) = outcome {
                return outcome.result;
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if let Some(error) = status.setup_error {
                    return Err(error);
                }
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(error));
                }
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

fn validate_managed_session_retry(
    state: &engine::CoreAgentState,
    admitted: Option<&AdmittedManagedSessionWorkflowTools>,
) -> Result<(), AgentApiError> {
    let Some(admitted) = admitted else {
        return Ok(());
    };
    match (
        state.workflow_tools.session_universe_id,
        state.workflow_tools.managed_creation_fingerprint.as_deref(),
    ) {
        (Some(actual_universe), Some(actual))
            if actual_universe == admitted.session_universe_id
                && actual == admitted.creation_fingerprint =>
        {
            Ok(())
        }
        (Some(_), Some(_)) => Err(AgentApiError::conflict(
            "managed-session controller, receiver, or tool declaration conflicts with durable creation state",
        )),
        _ => Err(AgentApiError::conflict(
            "existing standalone session cannot be reopened as a managed session",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, sync::Mutex};

    enum Step {
        Load(Result<Box<LoadedSession>, AgentApiError>),
        Running(Result<bool, AgentApiError>),
        Retry(Result<(), AgentApiError>),
        Status(Result<Option<Box<AgentSessionStatus>>, AgentApiError>),
    }

    struct ScriptedIo(Mutex<VecDeque<Step>>);

    impl ScriptedIo {
        fn new(steps: impl IntoIterator<Item = Step>) -> Self {
            Self(Mutex::new(steps.into_iter().collect()))
        }

        fn next(&self) -> Step {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected lifecycle I/O")
        }

        fn assert_drained(&self) {
            assert!(
                self.0.lock().unwrap().is_empty(),
                "expected lifecycle I/O was not performed"
            );
        }

        fn lifecycle(&self) -> SessionLifecycle<'_, Self> {
            SessionLifecycle {
                io: self,
                operation_timeout: Duration::from_secs(5),
                poll_interval: Duration::ZERO,
            }
        }
    }

    #[async_trait]
    impl SessionLifecycleIo for ScriptedIo {
        async fn load(&self, _: &SessionId) -> Result<LoadedSession, AgentApiError> {
            let Step::Load(result) = self.next() else {
                panic!("expected load");
            };
            result.map(|loaded| *loaded)
        }
        async fn is_running(&self, _: &SessionId) -> Result<bool, AgentApiError> {
            let Step::Running(result) = self.next() else {
                panic!("expected describe");
            };
            result
        }
        async fn retry_setup(&self, _: &SessionId) -> Result<(), AgentApiError> {
            let Step::Retry(result) = self.next() else {
                panic!("expected retry signal");
            };
            result
        }
        async fn status(&self, _: &SessionId) -> Result<Option<AgentSessionStatus>, AgentApiError> {
            let Step::Status(result) = self.next() else {
                panic!("expected status query");
            };
            result.map(|status| status.map(|status| *status))
        }
    }

    fn admitted() -> AdmittedManagedSessionWorkflowTools {
        ManagedSessionWorkflowTools::v1(None, Vec::new())
            .admit(uuid::Uuid::from_u128(1))
            .unwrap()
    }

    fn loaded(status: CoreAgentStatus, revision: u64, managed: bool) -> Box<LoadedSession> {
        let session_id = SessionId::new("session-lifecycle");
        let mut state = engine::CoreAgentState::new();
        state.lifecycle.status = status;
        state.lifecycle.config_revision = revision;
        if managed {
            let admitted = admitted();
            state.workflow_tools.session_universe_id = Some(admitted.session_universe_id);
            state.workflow_tools.managed_creation_fingerprint = Some(admitted.creation_fingerprint);
        }
        Box::new(LoadedSession {
            state,
            record: engine::storage::SessionRecord {
                session_id: session_id.clone(),
                display_name: None,
                metadata: BTreeMap::new(),
                lifecycle_status: Default::default(),
                closed_at_seq: None,
                closed_at_ms: None,
                retention_root_session_id: session_id,
                delete_after_close_ms: None,
                delete_at_ms: None,
                managed,
                head: None,
                source_session_id: None,
                source_seq: None,
                origin: None,
                created_at_ms: 0,
                updated_at_ms: 0,
            },
        })
    }

    fn status(ready: bool) -> Box<AgentSessionStatus> {
        Box::new(AgentSessionStatus {
            session_id: "session-lifecycle".into(),
            initialized: true,
            ready,
            setup_error: None,
            pending_admissions: 0,
            pending_tool_batch_resumes: 0,
            active_waits: 0,
            pending_emissions: 0,
            active_run: None,
            queued_runs: Vec::new(),
            completed_runs: Vec::new(),
            admission_failures: Vec::new(),
            last_error: None,
            bootstrap_failed: false,
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stored_sessions_validate_before_retry_and_reuse_the_loaded_response_state() {
        let session_id = SessionId::new("session-lifecycle");
        for closed in [false, true] {
            for managed_matches in [false, true] {
                let initial_status = if closed {
                    CoreAgentStatus::Closed
                } else {
                    CoreAgentStatus::Open
                };
                let mut steps = vec![Step::Load(Ok(loaded(initial_status, 1, managed_matches)))];
                if managed_matches && !closed {
                    steps.extend([
                        Step::Retry(Ok(())),
                        Step::Status(Ok(Some(status(true)))),
                        Step::Load(Ok(loaded(CoreAgentStatus::Open, 2, true))),
                    ]);
                }
                let io = ScriptedIo::new(steps);
                let result = io
                    .lifecycle()
                    .recover_existing(&session_id, Some(&admitted()))
                    .await;
                if managed_matches {
                    let loaded = result.unwrap().expect("recovered session");
                    assert_eq!(
                        loaded.state.lifecycle.config_revision,
                        if closed { 1 } else { 2 }
                    );
                } else {
                    assert_eq!(
                        result.err().expect("mismatch").kind,
                        AgentApiErrorKind::Conflict
                    );
                }
                io.assert_drained();
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn running_workflow_recovers_without_fresh_creation_and_validates_after_readiness() {
        let session_id = SessionId::new("session-lifecycle");
        for row_exists in [false, true] {
            for managed_matches in [false, true] {
                let initial = if row_exists {
                    Ok(loaded(CoreAgentStatus::New, 0, false))
                } else {
                    Err(AgentApiError::not_found("not yet stored"))
                };
                let io = ScriptedIo::new([
                    Step::Load(initial),
                    Step::Running(Ok(true)),
                    Step::Retry(Ok(())),
                    Step::Status(Ok(None)),
                    Step::Status(Ok(Some(status(false)))),
                    Step::Status(Ok(Some(status(true)))),
                    Step::Load(Ok(loaded(CoreAgentStatus::Open, 7, managed_matches))),
                ]);
                // The recovered value makes the caller return before profile resolution.
                let result = io
                    .lifecycle()
                    .recover_existing(&session_id, Some(&admitted()))
                    .await;
                if managed_matches {
                    assert_eq!(
                        result
                            .unwrap()
                            .expect("recovered original intent")
                            .state
                            .lifecycle
                            .config_revision,
                        7
                    );
                } else {
                    assert_eq!(
                        result.err().expect("mismatch after setup").kind,
                        AgentApiErrorKind::Conflict
                    );
                }
                io.assert_drained();
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_or_new_sessions_without_a_running_workflow_continue_to_creation() {
        for initial in [
            Err(AgentApiError::not_found("missing")),
            Ok(loaded(CoreAgentStatus::New, 0, false)),
        ] {
            let io = ScriptedIo::new([Step::Load(initial), Step::Running(Ok(false))]);
            assert!(
                io.lifecycle()
                    .recover_existing(&SessionId::new("session-lifecycle"), None)
                    .await
                    .unwrap()
                    .is_none()
            );
            io.assert_drained();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_start_conflicts_recover_stored_sessions_but_preserve_missing_row_errors() {
        let session_id = SessionId::new("session-lifecycle");
        for closed in [false, true] {
            let initial_status = if closed {
                CoreAgentStatus::Closed
            } else {
                CoreAgentStatus::Open
            };
            let mut steps = vec![Step::Load(Ok(loaded(initial_status, 1, true)))];
            if !closed {
                steps.extend([
                    Step::Retry(Ok(())),
                    Step::Status(Ok(Some(status(true)))),
                    Step::Load(Ok(loaded(CoreAgentStatus::Open, 2, true))),
                ]);
            }
            let io = ScriptedIo::new(steps);
            let recovered = io
                .lifecycle()
                .recover_conflict(&session_id, Some(&admitted()))
                .await
                .unwrap();
            assert_eq!(
                recovered.state.lifecycle.config_revision,
                if closed { 1 } else { 2 }
            );
            io.assert_drained();
        }
        let missing = AgentApiError::not_found("row not yet stored");
        let io = ScriptedIo::new([Step::Load(Err(missing.clone()))]);
        assert_eq!(
            io.lifecycle()
                .recover_conflict(&session_id, None)
                .await
                .err()
                .unwrap(),
            missing
        );
        io.assert_drained();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn readiness_checks_errors_before_ready_and_loads_only_once_after_success() {
        let session_id = SessionId::new("session-lifecycle");
        let setup_error = AgentApiError::invalid_request("setup failed");
        let mut setup = status(true);
        setup.setup_error = Some(setup_error.clone());
        setup.last_error = Some("secondary error".into());
        let mut failed = status(true);
        failed.last_error = Some("workflow failed".into());
        for (query, expected) in [
            (Ok(Some(setup)), setup_error),
            (Ok(Some(failed)), AgentApiError::internal("workflow failed")),
            (
                Err(AgentApiError::not_found("query failed")),
                AgentApiError::not_found("query failed"),
            ),
        ] {
            let io = ScriptedIo::new([Step::Status(query)]);
            assert_eq!(
                io.lifecycle()
                    .wait_for_open_session(&session_id)
                    .await
                    .err()
                    .unwrap(),
                expected
            );
            io.assert_drained();
        }
        let io = ScriptedIo::new([
            Step::Status(Ok(Some(status(true)))),
            Step::Load(Ok(loaded(CoreAgentStatus::Open, 9, false))),
        ]);
        assert_eq!(
            io.lifecycle()
                .wait_for_open_session(&session_id)
                .await
                .unwrap()
                .state
                .lifecycle
                .config_revision,
            9
        );
        io.assert_drained();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recovery_propagates_load_describe_and_signal_errors_without_further_io() {
        let error = AgentApiError::internal("unavailable");
        for steps in [
            vec![Step::Load(Err(error.clone()))],
            vec![
                Step::Load(Err(AgentApiError::not_found("missing"))),
                Step::Running(Err(error.clone())),
            ],
            vec![
                Step::Load(Ok(loaded(CoreAgentStatus::Open, 1, false))),
                Step::Retry(Err(error.clone())),
            ],
        ] {
            let io = ScriptedIo::new(steps);
            assert_eq!(
                io.lifecycle()
                    .recover_existing(&SessionId::new("session-lifecycle"), None)
                    .await
                    .err()
                    .unwrap(),
                error
            );
            io.assert_drained();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn readiness_timeout_does_not_load_session_state() {
        // At most one query if the first elapsed-time reading is exactly zero.
        let io = ScriptedIo::new([Step::Status(Ok(None))]);
        let lifecycle = SessionLifecycle {
            io: &io,
            operation_timeout: Duration::ZERO,
            poll_interval: Duration::from_millis(1),
        };
        let error = lifecycle
            .wait_for_open_session(&SessionId::new("session-lifecycle"))
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind, AgentApiErrorKind::Internal);
        assert!(
            error
                .message
                .contains("timed out waiting for agent session to open")
        );
    }
    #[test]
    fn managed_session_retry_requires_the_durable_creation_fingerprint() {
        let universe_id = uuid::Uuid::from_u128(1);
        let declaration = engine::ManagedSessionWorkflowTools::v1(
            Some(engine::WorkflowEndpointRef {
                workflow_id: "global controller/work-1".to_owned(),
                workflow_kind: "agent_work".to_owned(),
            }),
            Vec::new(),
        );
        let admitted = declaration.admit(universe_id).expect("admit");
        let mut state = engine::CoreAgentState::new();
        state.workflow_tools.session_universe_id = Some(universe_id);
        state.workflow_tools.managed_creation_fingerprint = Some(
            declaration
                .creation_fingerprint(universe_id)
                .expect("creation fingerprint"),
        );
        validate_managed_session_retry(&state, Some(&admitted)).expect("matching retry");

        let conflicting = engine::ManagedSessionWorkflowTools::v1(
            Some(engine::WorkflowEndpointRef {
                workflow_id: "another controller".to_owned(),
                workflow_kind: "agent_work".to_owned(),
            }),
            Vec::new(),
        );
        assert_eq!(
            validate_managed_session_retry(&state, Some(&conflicting.admit(universe_id).unwrap()))
                .expect_err("conflicting retry")
                .kind,
            AgentApiErrorKind::Conflict
        );
        assert_eq!(
            validate_managed_session_retry(&engine::CoreAgentState::new(), Some(&admitted))
                .expect_err("standalone session cannot become managed")
                .kind,
            AgentApiErrorKind::Conflict
        );
    }
}
