//! `api` gateway for the Temporal-backed agent workflow.

use std::{
    collections::BTreeMap,
    env,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use api::{
    AgentApiError, AgentApiErrorKind, AgentApiOutcome, AgentApiService, ClientCapabilities,
    ContextConfigInput as ApiContextConfigInput, ContextConfigPatchInput, FieldPatch,
    GenerationConfig, GenerationConfigPatch, InitializeParams, InitializeResponse, InputItem,
    InstructionsSource, ModelConfig, ReasoningEffort, RunCancelParams, RunCancelResponse,
    RunDefaultsConfig, RunDefaultsPatch, RunLimitsConfig, RunStartConfig, RunStartParams,
    RunStartResponse, RunView, ServerCapabilities, ServerInfo, SessionCloseParams,
    SessionCloseResponse, SessionConfigInput, SessionConfigPatchInput, SessionEventsReadParams,
    SessionEventsReadResponse, SessionReadParams, SessionReadResponse, SessionStartParams,
    SessionStartResponse, SessionUpdateParams, SessionUpdateResponse, SessionView,
};
use api_projection::{
    CoreAgentProjector, MAX_EVENT_PAGE_LIMIT, ProjectSession, api_kind_from_str, api_run_id,
    decode_dynamic_entry, event_cursor, event_page_limit, map_session_store_error,
    parse_api_run_id, read_all_session_entries, replay_core_agent_state,
};
use async_trait::async_trait;
use engine::{
    AnthropicMessagesRequestDefaults, BlobRef, CommandCodec, ContextConfigPatch, CoreAgentCommand,
    CoreAgentStatus, ModelProviderOptions, ModelSelection, OpenAiCompletionsRequestDefaults,
    OpenAiReasoningConfig, OpenAiResponsesRequestDefaults, OptionalConfigPatch, ProviderApiKind,
    ProviderRequestDefaults, RunConfig, RunConfigPatch, RunId, RunStatus, SessionConfig,
    SessionConfigPatch, SessionId, SubmissionId, TurnConfigPatch,
    storage::{BlobStore, BlobStoreError, ReadSessionEvents, SessionStore},
};
use store_pg::PgStore;
use temporalio_client::{
    Client, WorkflowHandle, WorkflowQueryOptions, WorkflowSignalOptions, WorkflowStartOptions,
    errors::WorkflowInteractionError, errors::WorkflowQueryError, errors::WorkflowStartError,
};

use crate::{
    AgentAdmission, AgentAdmissionFailure, AgentAdmissionFailureKind, AgentSessionArgs,
    AgentSessionStatus, AgentSessionWorkflow, DEFAULT_TASK_QUEUE, DEFAULT_TEMPORAL_NAMESPACE,
    DEFAULT_TEMPORAL_TARGET, connect_temporal, default_model_from_env, default_session_config,
    pg_store_from_env,
};

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);
const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(90);

pub struct GatewayAgentApiBuilder {
    client: Client,
    store: Arc<PgStore>,
    task_queue: String,
    default_model: ModelSelection,
    instructions_ref: Option<BlobRef>,
    max_steps_per_input: Option<u32>,
    continue_as_new_history_threshold: Option<u32>,
    poll_interval: Duration,
    operation_timeout: Duration,
}

impl GatewayAgentApiBuilder {
    pub fn with_task_queue(mut self, task_queue: impl Into<String>) -> Self {
        self.task_queue = task_queue.into();
        self
    }

    pub fn with_default_model(mut self, model: ModelSelection) -> Self {
        self.default_model = model;
        self
    }

    pub fn with_instructions_ref(mut self, instructions_ref: BlobRef) -> Self {
        self.instructions_ref = Some(instructions_ref);
        self
    }

    pub fn with_max_steps_per_input(mut self, max_steps: u32) -> Self {
        self.max_steps_per_input = Some(max_steps);
        self
    }

    pub fn with_continue_as_new_history_threshold(mut self, threshold: u32) -> Self {
        self.continue_as_new_history_threshold = Some(threshold);
        self
    }

    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }

    pub fn with_operation_timeout(mut self, operation_timeout: Duration) -> Self {
        self.operation_timeout = operation_timeout;
        self
    }

    pub fn build(self) -> GatewayAgentApi {
        GatewayAgentApi {
            client: self.client,
            store: self.store,
            task_queue: self.task_queue,
            default_model: self.default_model,
            instructions_ref: self.instructions_ref,
            max_steps_per_input: self.max_steps_per_input,
            continue_as_new_history_threshold: self.continue_as_new_history_threshold,
            poll_interval: self.poll_interval,
            operation_timeout: self.operation_timeout,
            metadata: RwLock::new(BTreeMap::new()),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct GatewaySessionMetadata {
    cwd: Option<String>,
}

pub struct GatewayAgentApi {
    client: Client,
    store: Arc<PgStore>,
    task_queue: String,
    default_model: ModelSelection,
    instructions_ref: Option<BlobRef>,
    max_steps_per_input: Option<u32>,
    continue_as_new_history_threshold: Option<u32>,
    poll_interval: Duration,
    operation_timeout: Duration,
    metadata: RwLock<BTreeMap<SessionId, GatewaySessionMetadata>>,
}

impl GatewayAgentApi {
    pub fn builder(client: Client, store: Arc<PgStore>) -> GatewayAgentApiBuilder {
        GatewayAgentApiBuilder {
            client,
            store,
            task_queue: DEFAULT_TASK_QUEUE.to_owned(),
            default_model: default_model_from_env(),
            instructions_ref: None,
            max_steps_per_input: Some(128),
            continue_as_new_history_threshold: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
        }
    }

    pub fn new(client: Client, store: Arc<PgStore>) -> Self {
        Self::builder(client, store).build()
    }

    pub async fn from_env() -> anyhow::Result<Self> {
        let temporal_target =
            env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
        let namespace = env::var("TEMPORAL_NAMESPACE")
            .unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());
        let task_queue =
            env::var("FORGE_TASK_QUEUE").unwrap_or_else(|_| DEFAULT_TASK_QUEUE.to_owned());
        let client = connect_temporal(&temporal_target, &namespace).await?;
        let store = pg_store_from_env().await?;
        Ok(Self::builder(client, store)
            .with_task_queue(task_queue)
            .build())
    }

    pub async fn open_or_start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        match self.start_session(params.clone()).await {
            Ok(outcome) => Ok(outcome),
            Err(error)
                if matches!(error.kind, AgentApiErrorKind::Conflict)
                    && params.session_id.is_some() =>
            {
                let session_id =
                    SessionId::try_new(params.session_id.expect("checked session id present"))
                        .map_err(|error| {
                            AgentApiError::invalid_request(format!("invalid session id: {error}"))
                        })?;
                self.write_session_metadata(
                    session_id.clone(),
                    GatewaySessionMetadata { cwd: params.cwd },
                )?;
                let session = self.wait_for_open_session(&session_id).await?;
                Ok(AgentApiOutcome::new(SessionStartResponse { session }))
            }
            Err(error) => Err(error),
        }
    }

    fn allocate_session_id(&self) -> SessionId {
        SessionId::new(format!("session_{}", uuid::Uuid::new_v4().simple()))
    }

    fn allocate_submission_id(&self) -> SubmissionId {
        SubmissionId::new(format!("submit_{}", uuid::Uuid::new_v4().simple()))
    }

    fn session_metadata(
        &self,
        session_id: &SessionId,
    ) -> Result<GatewaySessionMetadata, AgentApiError> {
        let metadata = self
            .metadata
            .read()
            .map_err(|_| AgentApiError::internal("gateway metadata lock poisoned"))?;
        Ok(metadata.get(session_id).cloned().unwrap_or_default())
    }

    fn write_session_metadata(
        &self,
        session_id: SessionId,
        metadata: GatewaySessionMetadata,
    ) -> Result<(), AgentApiError> {
        self.metadata
            .write()
            .map_err(|_| AgentApiError::internal("gateway metadata lock poisoned"))?
            .insert(session_id, metadata);
        Ok(())
    }

    fn workflow_args(
        &self,
        session_id: SessionId,
        session_config: SessionConfig,
    ) -> AgentSessionArgs {
        AgentSessionArgs {
            session_id,
            session_config,
            max_steps_per_input: self.max_steps_per_input,
            continue_as_new_history_threshold: self.continue_as_new_history_threshold,
        }
    }

    async fn session_config_for_start(
        &self,
        api_config: Option<SessionConfigInput>,
    ) -> Result<SessionConfig, AgentApiError> {
        let mut config =
            default_session_config(self.default_model.clone(), self.instructions_ref.clone());
        self.apply_session_config_input(&mut config, api_config)
            .await?;
        config
            .validate_provider_compatibility()
            .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
        Ok(config)
    }

    async fn apply_session_config_input(
        &self,
        config: &mut SessionConfig,
        api_config: Option<SessionConfigInput>,
    ) -> Result<(), AgentApiError> {
        let Some(api_config) = api_config else {
            return Ok(());
        };
        if let Some(instructions) = api_config.instructions {
            config.context.instructions_ref =
                Some(self.instructions_ref_from_source(instructions).await?);
        }
        if let Some(model) = api_config.model {
            let previous_api_kind = config.model.api_kind.clone();
            config.model = model_selection_from_api(model)?;
            if config.model.api_kind != previous_api_kind {
                config.turn.provider_request_defaults =
                    default_provider_request_defaults(&config.model.api_kind);
            }
        }
        apply_generation_config(config, api_config.generation)?;
        apply_context_config(&mut config.context, api_config.context);
        apply_run_defaults_config(&mut config.run, api_config.run_defaults);
        Ok(())
    }

    async fn instructions_ref_from_source(
        &self,
        source: InstructionsSource,
    ) -> Result<BlobRef, AgentApiError> {
        match source {
            InstructionsSource::Text { text } => self
                .store
                .put_bytes(text.into_bytes())
                .await
                .map_err(map_blob_store_error),
            InstructionsSource::BlobRef { blob_ref } => {
                let blob_ref = BlobRef::parse(blob_ref)
                    .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
                let exists = self
                    .store
                    .has_blob(&blob_ref)
                    .await
                    .map_err(map_blob_store_error)?;
                if exists {
                    Ok(blob_ref)
                } else {
                    Err(AgentApiError::invalid_request(format!(
                        "instructions blob not found: {blob_ref}"
                    )))
                }
            }
        }
    }

    async fn core_session_patch_from_api(
        &self,
        current: &SessionConfig,
        patch: SessionConfigPatchInput,
    ) -> Result<SessionConfigPatch, AgentApiError> {
        let instructions_ref = match patch.instructions {
            Some(FieldPatch::Set(source)) => Some(OptionalConfigPatch::Set(
                self.instructions_ref_from_source(source).await?,
            )),
            Some(FieldPatch::Clear) => Some(OptionalConfigPatch::Clear),
            None => None,
        };
        let model = patch.model.map(model_selection_from_api).transpose()?;
        let turn = turn_config_patch_from_api(current, patch.generation)?;
        Ok(SessionConfigPatch {
            model,
            run: run_config_patch_from_api(patch.run_defaults),
            turn,
            context: context_config_patch_from_api(instructions_ref, patch.context),
        })
    }

    fn projector(&self) -> CoreAgentProjector<'_> {
        CoreAgentProjector::new(self.store.as_ref())
    }

    fn workflow_handle(
        &self,
        session_id: &SessionId,
    ) -> WorkflowHandle<Client, AgentSessionWorkflow> {
        self.client
            .get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str())
    }

    async fn load_session_state(
        &self,
        session_id: &SessionId,
    ) -> Result<LoadedSession, AgentApiError> {
        let record = self
            .store
            .load_session(session_id)
            .await
            .map_err(map_session_store_error)?
            .ok_or_else(|| AgentApiError::not_found(format!("session not found: {session_id}")))?;
        let entries = read_all_session_entries(
            self.store.as_ref(),
            session_id,
            MAX_EVENT_PAGE_LIMIT as usize,
        )
        .await?;
        let state = replay_core_agent_state(&entries)?;
        Ok(LoadedSession {
            record,
            entries,
            state,
        })
    }

    async fn project_session_by_id(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionView, AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        let metadata = self.session_metadata(session_id)?;
        self.projector()
            .project_session(ProjectSession {
                session_id,
                state: &loaded.state,
                record: &loaded.record,
                entries: &loaded.entries,
                cwd: metadata.cwd,
            })
            .await
    }

    async fn project_run_by_id(
        &self,
        session_id: &SessionId,
        run_id: RunId,
        fallback_status: RunStatus,
    ) -> Result<RunView, AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        let status = loaded
            .state
            .runs
            .completed
            .iter()
            .find(|run| run.run_id == run_id)
            .map(|run| run.status)
            .or_else(|| loaded.state.runs.active.as_ref().map(|run| run.status))
            .unwrap_or(fallback_status);
        self.projector()
            .project_run(&loaded.entries, run_id, status)
            .await
    }

    async fn run_config_for_start(
        &self,
        session_id: &SessionId,
        api_config: Option<RunStartConfig>,
    ) -> Result<RunConfig, AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        let session_config = loaded.state.lifecycle.config.as_ref().ok_or_else(|| {
            AgentApiError::invalid_request(format!("session is not open: {session_id}"))
        })?;
        let mut run_config = session_config.run.clone();
        apply_run_start_config(&mut run_config, session_config, api_config)?;
        Ok(run_config)
    }

    async fn wait_for_open_session(
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

    async fn wait_for_config_revision(
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

    async fn wait_for_run_accepted(
        &self,
        session_id: &SessionId,
        submission_id: &SubmissionId,
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
            if let Some(active) = status
                .active_run
                .as_ref()
                .filter(|run| run.submission_id.as_ref() == Some(submission_id))
            {
                let run = self
                    .project_run_by_id(session_id, RunId::new(active.run_id), active.status)
                    .await?;
                if !run.input.is_empty() {
                    return Ok(run);
                }
            }
            if let Some(run) = status
                .completed_runs
                .iter()
                .rev()
                .find(|run| run.submission_id.as_ref() == Some(submission_id))
            {
                let run = self
                    .project_run_by_id(session_id, RunId::new(run.run_id), run.status)
                    .await?;
                if !run.input.is_empty() {
                    return Ok(run);
                }
            }
            if let Some(failure) = status
                .admission_failures
                .iter()
                .rev()
                .find(|failure| failure.submission_id.as_ref() == Some(submission_id))
            {
                return Err(map_admission_failure_to_api_error(failure));
            }
            if let Some(error) = status.last_error {
                return Err(AgentApiError::internal(format!(
                    "agent workflow reported error: {error}"
                )));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    async fn wait_for_closed_session(
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

    async fn wait_for_cancelled_run(
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

    async fn query_status_optional(
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
            Ok(status) => Ok(Some(status)),
            Err(WorkflowQueryError::NotFound(_)) => Ok(None),
            Err(error) => Err(map_workflow_query_error(error)),
        }
    }
}

struct LoadedSession {
    record: engine::storage::SessionRecord,
    entries: Vec<engine::CoreAgentEntry>,
    state: engine::CoreAgentState,
}

#[async_trait]
impl AgentApiService for GatewayAgentApi {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> Result<AgentApiOutcome<InitializeResponse>, AgentApiError> {
        let _capabilities = params.capabilities.unwrap_or(ClientCapabilities {
            experimental_api: false,
        });
        Ok(AgentApiOutcome::new(InitializeResponse {
            protocol_version: api::PROTOCOL_VERSION.to_owned(),
            server_info: ServerInfo {
                name: "forge-agent".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            capabilities: ServerCapabilities {
                notifications: false,
                history_read: true,
                event_log: true,
                local_execution: false,
            },
        }))
    }

    async fn start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        let SessionStartParams {
            session_id,
            cwd,
            config,
        } = params;
        let session_id = match session_id {
            Some(session_id) => SessionId::try_new(session_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid session id: {error}"))
            })?,
            None => self.allocate_session_id(),
        };
        let session_config = self.session_config_for_start(config).await?;
        self.write_session_metadata(session_id.clone(), GatewaySessionMetadata { cwd })?;
        self.client
            .start_workflow(
                AgentSessionWorkflow::run,
                self.workflow_args(session_id.clone(), session_config),
                WorkflowStartOptions::new(self.task_queue.clone(), session_id.as_str()).build(),
            )
            .await
            .map_err(map_workflow_start_error)?;
        let session = self.wait_for_open_session(&session_id).await?;
        Ok(AgentApiOutcome::new(SessionStartResponse { session }))
    }

    async fn update_session(
        &self,
        params: SessionUpdateParams,
    ) -> Result<AgentApiOutcome<SessionUpdateResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let loaded = self.load_session_state(&session_id).await?;
        if loaded.state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        if loaded.state.runs.active.is_some() || !loaded.state.runs.queued.is_empty() {
            return Err(AgentApiError::rejected(
                "session config can only change while no run is active or queued",
            ));
        }
        let current_config = loaded.state.lifecycle.config.as_ref().ok_or_else(|| {
            AgentApiError::invalid_request(format!("session is missing config: {session_id}"))
        })?;
        if let Some(expected) = params.expected_config_revision {
            let actual = loaded.state.lifecycle.config_revision;
            if expected != actual {
                return Err(AgentApiError::conflict(format!(
                    "expected config revision {expected}, got {actual}"
                )));
            }
        }
        let patch = self
            .core_session_patch_from_api(current_config, params.patch)
            .await?;
        if patch.is_empty() {
            return Ok(AgentApiOutcome::new(SessionUpdateResponse {
                session: self.project_session_by_id(&session_id).await?,
            }));
        }
        let baseline_failures = self
            .query_status_optional(&session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        let target_revision = loaded
            .state
            .lifecycle
            .config_revision
            .checked_add(1)
            .ok_or_else(|| AgentApiError::internal("config revision exhausted"))?;
        let command = engine::CoreAgentCodec
            .encode_command(&CoreAgentCommand::PatchSessionConfig {
                expected_revision: Some(loaded.state.lifecycle.config_revision),
                patch,
            })
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        self.workflow_handle(&session_id)
            .signal(
                AgentSessionWorkflow::submit_admission,
                AgentAdmission { command },
                WorkflowSignalOptions::default(),
            )
            .await
            .map_err(map_workflow_interaction_error)?;
        let session = self
            .wait_for_config_revision(&session_id, target_revision, baseline_failures)
            .await?;
        Ok(AgentApiOutcome::new(SessionUpdateResponse { session }))
    }

    async fn read_session(
        &self,
        params: SessionReadParams,
    ) -> Result<AgentApiOutcome<SessionReadResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        if let Some(status) = self.query_status_optional(&session_id).await? {
            if let Some(error) = status.last_error {
                return Err(AgentApiError::internal(format!(
                    "agent workflow reported error: {error}"
                )));
            }
        }
        let session = self.project_session_by_id(&session_id).await?;
        Ok(AgentApiOutcome::new(SessionReadResponse { session }))
    }

    async fn read_session_events(
        &self,
        params: SessionEventsReadParams,
    ) -> Result<AgentApiOutcome<SessionEventsReadResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        self.store
            .load_session(&session_id)
            .await
            .map_err(map_session_store_error)?
            .ok_or_else(|| AgentApiError::not_found(format!("session not found: {session_id}")))?;
        let limit = event_page_limit(params.limit)?;
        let page = self
            .store
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
                after: params.after.map(|cursor| engine::EventSeq::new(cursor.seq)),
                limit,
            })
            .await
            .map_err(map_session_store_error)?;
        let head_cursor = self
            .store
            .head(&session_id)
            .await
            .map_err(map_session_store_error)?
            .map(|position| event_cursor(position.seq));
        let codec = engine::CoreAgentCodec;
        let mut events = Vec::with_capacity(page.entries.len());
        for entry in &page.entries {
            let entry = decode_dynamic_entry(&codec, entry)?;
            events.push(self.projector().project_entry(&session_id, &entry).await?);
        }

        Ok(AgentApiOutcome::new(SessionEventsReadResponse {
            events,
            next_cursor: page.next_after.map(event_cursor),
            head_cursor,
            complete: page.complete,
            gap: None,
        }))
    }

    async fn close_session(
        &self,
        params: SessionCloseParams,
    ) -> Result<AgentApiOutcome<SessionCloseResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let loaded = self.load_session_state(&session_id).await?;
        if loaded.state.lifecycle.status == CoreAgentStatus::Closed {
            return Ok(AgentApiOutcome::new(SessionCloseResponse {
                session: self.project_session_by_id(&session_id).await?,
            }));
        }
        if loaded.state.runs.active.is_some() || !loaded.state.runs.queued.is_empty() {
            return Err(AgentApiError::rejected(
                "session cannot close with active work",
            ));
        }
        let command = engine::CoreAgentCodec
            .encode_command(&CoreAgentCommand::CloseSession)
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        self.workflow_handle(&session_id)
            .signal(
                AgentSessionWorkflow::submit_admission,
                AgentAdmission { command },
                WorkflowSignalOptions::default(),
            )
            .await
            .map_err(map_workflow_interaction_error)?;
        let session = self.wait_for_closed_session(&session_id).await?;
        Ok(AgentApiOutcome::new(SessionCloseResponse { session }))
    }

    async fn start_run(
        &self,
        params: RunStartParams,
    ) -> Result<AgentApiOutcome<RunStartResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let submission_id = self.allocate_submission_id();
        let run_config = self
            .run_config_for_start(&session_id, params.config)
            .await?;
        let input_ref = run_input_ref_from_api(self.store.as_ref(), &params.input).await?;
        let command = engine::CoreAgentCodec
            .encode_command(&CoreAgentCommand::RequestRun {
                submission_id: Some(submission_id.clone()),
                input_ref,
                run_config,
            })
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        self.workflow_handle(&session_id)
            .signal(
                AgentSessionWorkflow::submit_admission,
                AgentAdmission { command },
                WorkflowSignalOptions::default(),
            )
            .await
            .map_err(map_workflow_interaction_error)?;
        let run = self
            .wait_for_run_accepted(&session_id, &submission_id)
            .await?;
        Ok(AgentApiOutcome::new(RunStartResponse { run }))
    }

    async fn cancel_run(
        &self,
        params: RunCancelParams,
    ) -> Result<AgentApiOutcome<RunCancelResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let requested_run_id = parse_api_run_id(&params.run_id)?;
        let loaded = self.load_session_state(&session_id).await?;
        match loaded.state.runs.active.as_ref() {
            Some(active)
                if active.run_id == requested_run_id && active.status == RunStatus::Active => {}
            Some(active) if active.run_id == requested_run_id => {
                return Err(AgentApiError::rejected(format!(
                    "run is not cancellable: {}",
                    params.run_id
                )));
            }
            _ if loaded
                .state
                .runs
                .completed
                .iter()
                .any(|run| run.run_id == requested_run_id) =>
            {
                return Err(AgentApiError::rejected(format!(
                    "run is already terminal: {}",
                    params.run_id
                )));
            }
            _ => {
                return Err(AgentApiError::not_found(format!(
                    "run not found: {}",
                    params.run_id
                )));
            }
        }
        let command = engine::CoreAgentCodec
            .encode_command(&CoreAgentCommand::RequestRunCancellation)
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        self.workflow_handle(&session_id)
            .signal(
                AgentSessionWorkflow::submit_admission,
                AgentAdmission { command },
                WorkflowSignalOptions::default(),
            )
            .await
            .map_err(map_workflow_interaction_error)?;
        let run = self
            .wait_for_cancelled_run(&session_id, requested_run_id)
            .await?;
        Ok(AgentApiOutcome::new(RunCancelResponse { run }))
    }
}

async fn run_input_ref_from_api(
    store: &dyn BlobStore,
    input: &[InputItem],
) -> Result<BlobRef, AgentApiError> {
    if let [InputItem::TextRef { blob_ref }] = input {
        let blob_ref = parse_blob_ref(blob_ref)?;
        let text = store
            .read_text(&blob_ref)
            .await
            .map_err(map_input_blob_store_error)?;
        if text.trim().is_empty() {
            return Err(empty_run_input_error());
        }
        return Ok(blob_ref);
    }

    let mut parts = Vec::new();
    for item in input {
        match item {
            InputItem::Text { text } => {
                let text = text.trim();
                if !text.is_empty() {
                    parts.push(text.to_owned());
                }
            }
            InputItem::TextRef { blob_ref } => {
                let blob_ref = parse_blob_ref(blob_ref)?;
                let text = store
                    .read_text(&blob_ref)
                    .await
                    .map_err(map_input_blob_store_error)?;
                let text = text.trim();
                if !text.is_empty() {
                    parts.push(text.to_owned());
                }
            }
        }
    }

    if parts.is_empty() {
        return Err(empty_run_input_error());
    }
    store
        .put_bytes(parts.join("\n\n").into_bytes())
        .await
        .map_err(map_blob_store_error)
}

fn parse_blob_ref(value: &str) -> Result<BlobRef, AgentApiError> {
    BlobRef::parse(value).map_err(|error| AgentApiError::invalid_request(error.to_string()))
}

fn empty_run_input_error() -> AgentApiError {
    AgentApiError::invalid_request("run/start input must contain at least one non-empty text item")
}

fn apply_generation_config(
    config: &mut SessionConfig,
    generation: Option<GenerationConfig>,
) -> Result<(), AgentApiError> {
    let Some(generation) = generation else {
        return Ok(());
    };
    if let Some(max_output_tokens) = generation.max_output_tokens {
        config.turn.max_output_tokens = Some(max_output_tokens);
    }
    if let Some(effort) = generation.reasoning_effort {
        config.turn.provider_request_defaults = provider_defaults_with_reasoning(
            &config.model.api_kind,
            &config.turn.provider_request_defaults,
            effort,
        )?;
    }
    Ok(())
}

fn apply_context_config(
    config: &mut engine::ContextConfig,
    context: Option<ApiContextConfigInput>,
) {
    let Some(context) = context else {
        return;
    };
    if let Some(max_context_tokens) = context.max_context_tokens {
        config.max_context_tokens = Some(max_context_tokens);
    }
    if let Some(target_context_tokens) = context.target_context_tokens {
        config.target_context_tokens = Some(target_context_tokens);
    }
    if let Some(reserve_output_tokens) = context.reserve_output_tokens {
        config.reserve_output_tokens = Some(reserve_output_tokens);
    }
    if let Some(compaction_enabled) = context.compaction_enabled {
        config.compaction_enabled = compaction_enabled;
    }
}

fn apply_run_defaults_config(config: &mut RunConfig, run_defaults: Option<RunDefaultsConfig>) {
    let Some(run_defaults) = run_defaults else {
        return;
    };
    if let Some(max_turns) = run_defaults.max_turns {
        config.max_turns = Some(max_turns);
    }
    if let Some(max_tool_rounds) = run_defaults.max_tool_rounds {
        config.max_tool_rounds = Some(max_tool_rounds);
    }
}

fn apply_run_start_config(
    run_config: &mut RunConfig,
    session_config: &SessionConfig,
    api_config: Option<RunStartConfig>,
) -> Result<(), AgentApiError> {
    let Some(api_config) = api_config else {
        return Ok(());
    };
    let effective_api_kind = if let Some(model) = api_config.model {
        let model = model_selection_from_api(model)?;
        let api_kind = model.api_kind.clone();
        run_config.model_override = Some(model);
        api_kind
    } else {
        session_config.model.api_kind.clone()
    };
    if let Some(generation) = api_config.generation {
        if let Some(max_output_tokens) = generation.max_output_tokens {
            run_config.max_output_tokens = Some(max_output_tokens);
        }
        if let Some(effort) = generation.reasoning_effort {
            run_config.provider_request_defaults = Some(provider_defaults_with_reasoning(
                &effective_api_kind,
                &session_config.turn.provider_request_defaults,
                effort,
            )?);
        }
    }
    if let Some(limits) = api_config.limits {
        apply_run_limits_config(run_config, limits);
    }
    run_config
        .validate_provider_compatibility(&session_config.model.api_kind)
        .map_err(|error| AgentApiError::invalid_request(error.to_string()))
}

fn apply_run_limits_config(run_config: &mut RunConfig, limits: RunLimitsConfig) {
    if let Some(max_turns) = limits.max_turns {
        run_config.max_turns = Some(max_turns);
    }
    if let Some(max_tool_rounds) = limits.max_tool_rounds {
        run_config.max_tool_rounds = Some(max_tool_rounds);
    }
}

fn run_config_patch_from_api(patch: Option<RunDefaultsPatch>) -> RunConfigPatch {
    let Some(patch) = patch else {
        return RunConfigPatch::default();
    };
    RunConfigPatch {
        max_turns: patch.max_turns.map(optional_patch_from_api),
        max_tool_rounds: patch.max_tool_rounds.map(optional_patch_from_api),
        ..RunConfigPatch::default()
    }
}

fn turn_config_patch_from_api(
    current: &SessionConfig,
    patch: Option<GenerationConfigPatch>,
) -> Result<TurnConfigPatch, AgentApiError> {
    let Some(patch) = patch else {
        return Ok(TurnConfigPatch::default());
    };
    let provider_request_defaults = patch
        .reasoning_effort
        .map(|effort| {
            provider_defaults_with_reasoning(
                &current.model.api_kind,
                &current.turn.provider_request_defaults,
                effort,
            )
        })
        .transpose()?;
    Ok(TurnConfigPatch {
        max_output_tokens: patch.max_output_tokens.map(optional_patch_from_api),
        provider_request_defaults,
    })
}

fn context_config_patch_from_api(
    instructions_ref: Option<OptionalConfigPatch<BlobRef>>,
    patch: Option<ContextConfigPatchInput>,
) -> ContextConfigPatch {
    let Some(patch) = patch else {
        return ContextConfigPatch {
            instructions_ref,
            ..ContextConfigPatch::default()
        };
    };
    ContextConfigPatch {
        instructions_ref,
        max_context_tokens: patch.max_context_tokens.map(optional_patch_from_api),
        target_context_tokens: patch.target_context_tokens.map(optional_patch_from_api),
        reserve_output_tokens: patch.reserve_output_tokens.map(optional_patch_from_api),
        compaction_enabled: patch.compaction_enabled,
    }
}

fn optional_patch_from_api<T>(patch: FieldPatch<T>) -> OptionalConfigPatch<T> {
    match patch {
        FieldPatch::Set(value) => OptionalConfigPatch::Set(value),
        FieldPatch::Clear => OptionalConfigPatch::Clear,
    }
}

fn model_selection_from_api(model: ModelConfig) -> Result<ModelSelection, AgentApiError> {
    Ok(ModelSelection {
        api_kind: api_kind_from_str(&model.api_kind)?,
        provider_id: model.provider_id,
        model: model.model,
        options: ModelProviderOptions::None,
    })
}

fn default_provider_request_defaults(api_kind: &ProviderApiKind) -> ProviderRequestDefaults {
    match api_kind {
        ProviderApiKind::OpenAiResponses => {
            ProviderRequestDefaults::OpenAiResponses(OpenAiResponsesRequestDefaults::default())
        }
        ProviderApiKind::AnthropicMessages => {
            ProviderRequestDefaults::AnthropicMessages(AnthropicMessagesRequestDefaults::default())
        }
        ProviderApiKind::OpenAiCompletions => {
            ProviderRequestDefaults::OpenAiCompletions(OpenAiCompletionsRequestDefaults::default())
        }
    }
}

fn provider_defaults_with_reasoning(
    api_kind: &ProviderApiKind,
    base: &ProviderRequestDefaults,
    effort: ReasoningEffort,
) -> Result<ProviderRequestDefaults, AgentApiError> {
    if api_kind != &ProviderApiKind::OpenAiResponses {
        return Err(AgentApiError::invalid_request(
            "reasoning effort is only supported for openai:responses",
        ));
    }
    let mut defaults = match base {
        ProviderRequestDefaults::OpenAiResponses(defaults) => defaults.clone(),
        ProviderRequestDefaults::None => OpenAiResponsesRequestDefaults::default(),
        other => {
            return Err(AgentApiError::invalid_request(format!(
                "request defaults {other:?} do not match openai:responses"
            )));
        }
    };
    defaults.reasoning = match effort {
        ReasoningEffort::None => None,
        ReasoningEffort::Low => Some(openai_reasoning("low")),
        ReasoningEffort::Medium => Some(openai_reasoning("medium")),
        ReasoningEffort::High => Some(openai_reasoning("high")),
    };
    Ok(ProviderRequestDefaults::OpenAiResponses(defaults))
}

fn openai_reasoning(effort: &str) -> OpenAiReasoningConfig {
    OpenAiReasoningConfig {
        effort: Some(effort.to_owned()),
        summary: Some("auto".to_owned()),
        extra: BTreeMap::new(),
    }
}

fn is_not_found(error: &AgentApiError) -> bool {
    matches!(error.kind, AgentApiErrorKind::NotFound)
}

fn map_admission_failure_to_api_error(failure: &AgentAdmissionFailure) -> AgentApiError {
    match failure.kind {
        AgentAdmissionFailureKind::InvalidCommand => {
            AgentApiError::invalid_request(failure.message.clone())
        }
        AgentAdmissionFailureKind::RejectedCommand => {
            AgentApiError::rejected(failure.message.clone())
        }
    }
}

fn map_blob_store_error(error: BlobStoreError) -> AgentApiError {
    match error {
        BlobStoreError::NotFound { blob_ref } => {
            AgentApiError::internal(format!("stored run input blob disappeared: {blob_ref}"))
        }
        BlobStoreError::Store { message } => AgentApiError::internal(message),
    }
}

fn map_input_blob_store_error(error: BlobStoreError) -> AgentApiError {
    match error {
        BlobStoreError::NotFound { blob_ref } => {
            AgentApiError::invalid_request(format!("run/start input blob not found: {blob_ref}"))
        }
        BlobStoreError::Store { message } => AgentApiError::invalid_request(message),
    }
}

fn map_workflow_start_error(error: WorkflowStartError) -> AgentApiError {
    match error {
        WorkflowStartError::AlreadyStarted { .. } => {
            AgentApiError::conflict("agent session workflow already exists")
        }
        WorkflowStartError::PayloadConversion(error) => AgentApiError::internal(error.to_string()),
        WorkflowStartError::Rpc(status) => AgentApiError::internal(status.to_string()),
        _ => AgentApiError::internal(error.to_string()),
    }
}

fn map_workflow_query_error(error: WorkflowQueryError) -> AgentApiError {
    match error {
        WorkflowQueryError::NotFound(_) => AgentApiError::not_found("agent workflow not found"),
        WorkflowQueryError::Rejected(rejection) => {
            AgentApiError::internal(format!("{rejection:?}"))
        }
        WorkflowQueryError::PayloadConversion(error) => AgentApiError::internal(error.to_string()),
        WorkflowQueryError::Rpc(status) => AgentApiError::internal(status.to_string()),
        WorkflowQueryError::Other(error) => AgentApiError::internal(error.to_string()),
        _ => AgentApiError::internal(error.to_string()),
    }
}

fn map_workflow_interaction_error(error: WorkflowInteractionError) -> AgentApiError {
    match error {
        WorkflowInteractionError::NotFound(_) => {
            AgentApiError::not_found("agent workflow not found")
        }
        WorkflowInteractionError::PayloadConversion(error) => {
            AgentApiError::internal(error.to_string())
        }
        WorkflowInteractionError::Rpc(status) => AgentApiError::internal(status.to_string()),
        WorkflowInteractionError::Other(error) => AgentApiError::internal(error.to_string()),
        _ => AgentApiError::internal(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_failure_mapping_uses_gateway_error_kinds() {
        assert_eq!(
            map_admission_failure_to_api_error(&failure(AgentAdmissionFailureKind::InvalidCommand))
                .kind,
            AgentApiErrorKind::InvalidRequest
        );
        assert_eq!(
            map_admission_failure_to_api_error(&failure(
                AgentAdmissionFailureKind::RejectedCommand
            ))
            .kind,
            AgentApiErrorKind::Rejected
        );
    }

    #[test]
    fn session_start_config_maps_reasoning_and_max_output_tokens() {
        let mut config = default_session_config(openai_model(), None);

        apply_generation_config(
            &mut config,
            Some(GenerationConfig {
                max_output_tokens: Some(2048),
                reasoning_effort: Some(ReasoningEffort::High),
            }),
        )
        .expect("apply config");

        assert_eq!(config.turn.max_output_tokens, Some(2048));
        let ProviderRequestDefaults::OpenAiResponses(defaults) =
            config.turn.provider_request_defaults
        else {
            panic!("expected OpenAI Responses defaults");
        };
        let reasoning = defaults.reasoning.expect("reasoning");
        assert_eq!(reasoning.effort.as_deref(), Some("high"));
        assert_eq!(reasoning.summary.as_deref(), Some("auto"));
    }

    #[test]
    fn run_start_config_maps_model_and_generation_overrides() {
        let session_config = default_session_config(openai_model(), None);
        let mut run_config = RunConfig::default();

        apply_run_start_config(
            &mut run_config,
            &session_config,
            Some(RunStartConfig {
                model: Some(ModelConfig {
                    provider_id: "openai".to_owned(),
                    api_kind: "openai:responses".to_owned(),
                    model: "gpt-5.5-mini".to_owned(),
                }),
                generation: Some(GenerationConfig {
                    max_output_tokens: Some(1024),
                    reasoning_effort: Some(ReasoningEffort::Medium),
                }),
                limits: None,
            }),
        )
        .expect("apply run config");

        assert_eq!(
            run_config
                .model_override
                .as_ref()
                .map(|model| model.model.as_str()),
            Some("gpt-5.5-mini")
        );
        assert_eq!(run_config.max_output_tokens, Some(1024));
        let ProviderRequestDefaults::OpenAiResponses(defaults) = run_config
            .provider_request_defaults
            .expect("request defaults")
        else {
            panic!("expected OpenAI Responses defaults");
        };
        assert_eq!(
            defaults.reasoning.expect("reasoning").effort.as_deref(),
            Some("medium")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_input_ref_from_api_preserves_single_text_ref() {
        let store = engine::storage::InMemoryBlobStore::new();
        let blob_ref = store.insert_text("hello from cas").await;

        let input_ref = run_input_ref_from_api(
            &store,
            &[InputItem::TextRef {
                blob_ref: blob_ref.as_str().to_owned(),
            }],
        )
        .await
        .expect("input ref");

        assert_eq!(input_ref, blob_ref);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_input_ref_from_api_joins_text_and_refs() {
        let store = engine::storage::InMemoryBlobStore::new();
        let blob_ref = store.insert_text(" second ").await;

        let input_ref = run_input_ref_from_api(
            &store,
            &[
                InputItem::Text {
                    text: " first ".to_owned(),
                },
                InputItem::TextRef {
                    blob_ref: blob_ref.as_str().to_owned(),
                },
            ],
        )
        .await
        .expect("input ref");

        assert_ne!(input_ref, blob_ref);
        assert_eq!(
            store.read_text(&input_ref).await.expect("stored input"),
            "first\n\nsecond"
        );
    }

    fn failure(kind: AgentAdmissionFailureKind) -> AgentAdmissionFailure {
        AgentAdmissionFailure {
            submission_id: Some(SubmissionId::new("submit_test")),
            kind,
            message: "admission failed".to_owned(),
        }
    }

    fn openai_model() -> ModelSelection {
        ModelSelection {
            api_kind: ProviderApiKind::OpenAiResponses,
            provider_id: "openai".to_owned(),
            model: "gpt-5.5".to_owned(),
            options: ModelProviderOptions::None,
        }
    }
}
