//! `api` gateway for the Temporal-backed agent workflow.

mod api_config;
mod blobs;
mod errors;
mod input;
mod parse;
mod prompts;
mod skills;
mod vfs_api;
mod workflow;

#[cfg(test)]
use api_config::*;
use blobs::{get_blob, has_blobs, put_blob, put_blobs};
use errors::*;
use input::run_input_from_api;
use parse::*;
use skills::{
    active_skill_catalog_ref, active_skill_ids, active_skill_ids_after_remove,
    active_skill_ids_after_upsert, skill_activation_context_input,
};
#[cfg(test)]
use skills::{read_skill_doc_for_activation_from_vfs, skill_active_response, skill_list_response};
use vfs_api::{commit_vfs_snapshot, read_vfs_snapshot, vfs_workspace_view};

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    sync::{Arc, RwLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use api::{
    AgentApiError, AgentApiErrorKind, AgentApiOutcome, AgentApiService, BlobGetParams,
    BlobGetResponse, BlobHasItem, BlobHasManyParams, BlobHasManyResponse, BlobPutManyParams,
    BlobPutManyResponse, BlobPutParams, BlobPutResponse, ClientCapabilities, CompactionPolicyInput,
    ContextCompactParams, ContextCompactResponse, ContextConfigInput as ApiContextConfigInput,
    ContextConfigPatchInput, FieldPatch, GenerationConfig, GenerationConfigPatch, InitializeParams,
    InitializeResponse, InputItem, ModelConfig, PromptInstructionView, PromptsActiveParams,
    PromptsActiveResponse, ReasoningEffort, RunCancelParams, RunCancelResponse, RunDefaultsConfig,
    RunDefaultsPatch, RunLimitsConfig, RunStartConfig, RunStartParams, RunStartResponse, RunView,
    ServerCapabilities, ServerInfo, SessionCloseParams, SessionCloseResponse, SessionConfigInput,
    SessionConfigPatchInput, SessionEventsReadParams, SessionEventsReadResponse, SessionReadParams,
    SessionReadResponse, SessionStartParams, SessionStartResponse, SessionUpdateParams,
    SessionUpdateResponse, SessionView, SkillActivateParams, SkillActivateResponse,
    SkillActivationScope as ApiSkillActivationScope,
    SkillActivationSource as ApiSkillActivationSource, SkillActivationView, SkillActiveParams,
    SkillActiveResponse, SkillDeactivateParams, SkillDeactivateResponse, SkillListItem,
    SkillListParams, SkillListResponse, ToolConfigInput, ToolConfigPatchInput,
    VfsMountAccess as ApiVfsMountAccess, VfsMountDeleteParams, VfsMountDeleteResponse,
    VfsMountListParams, VfsMountListResponse, VfsMountPutParams, VfsMountPutResponse,
    VfsMountSourceInput, VfsMountSourceView, VfsMountView, VfsSnapshotCommitParams,
    VfsSnapshotCommitResponse, VfsSnapshotReadParams, VfsSnapshotReadResponse,
    VfsWorkspaceCreateParams, VfsWorkspaceCreateResponse, VfsWorkspaceDeleteParams,
    VfsWorkspaceDeleteResponse, VfsWorkspaceReadParams, VfsWorkspaceReadResponse,
    VfsWorkspaceUpdateParams, VfsWorkspaceUpdateResponse, VfsWorkspaceView,
};
use api_projection::{
    CoreAgentProjector, MAX_EVENT_PAGE_LIMIT, ProjectSession, api_kind_from_str, api_run_id,
    decode_dynamic_entry, event_cursor, event_page_limit, map_session_store_error,
    parse_api_run_id, read_all_session_entries, replay_core_agent_state,
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use engine::{
    AnthropicMessagesRequestDefaults, BlobRef, CommandCodec, CompactionPolicy, ContextConfigPatch,
    ContextEntry, ContextEntryInput, ContextEntryKey, ContextEntryKind, ContextMessageRole,
    CoreAgentCommand, CoreAgentStatus, HostToolMode, ModelProviderOptions, ModelSelection,
    OpenAiCompletionsRequestDefaults, OpenAiReasoningConfig, OpenAiResponsesRequestDefaults,
    OptionalConfigPatch, ProviderApiKind, ProviderRequestDefaults, RunConfig, RunConfigPatch,
    RunId, RunStatus, SKILL_ACTIVATION_PROVIDER_KIND_RUN, SKILL_ACTIVATION_PROVIDER_KIND_SESSION,
    SKILL_CATALOG_CONTEXT_KEY, SessionConfig, SessionConfigPatch, SessionId, SkillId, SubmissionId,
    TurnConfigPatch, skill_activation_context_key,
    storage::{BlobStore, BlobStoreError, ReadSessionEvents, SessionStore},
};
use store_pg::PgStore;
use temporalio_client::{
    Client, WorkflowHandle, WorkflowQueryOptions, WorkflowSignalOptions, WorkflowStartOptions,
    errors::WorkflowInteractionError, errors::WorkflowQueryError, errors::WorkflowStartError,
};
use tools::{
    host::{
        HostToolTargets,
        fs::{FileSystem, FsPath, MountedVfsFileSystem},
    },
    runtime::{ToolDocument, ToolTarget},
    skills::{
        SkillCatalogSnapshot, SkillLocation, SkillMetadata, conventional_vfs_skill_root_specs,
        prepare_skill_catalog_publication, resolve_mounted_vfs_skill_roots,
        skill_catalog_context_input,
    },
    toolset::{ResolvedToolset, ToolsetConfig, ToolsetEnvironment, resolve_toolset},
    web::search::OpenAiResponsesWebSearchConfig,
};
use vfs::{
    CompareAndSetVfsWorkspaceHead, CreateVfsWorkspaceRecord, VfsCatalogError, VfsMountAccess,
    VfsMountRecord, VfsMountSource, VfsMountStore, VfsPath, VfsSnapshotRecord, VfsSnapshotSource,
    VfsSnapshotStore, VfsWorkspaceId, VfsWorkspaceRecord, VfsWorkspaceStore,
};

use super::{
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
                    GatewaySessionMetadata {
                        cwd: params.cwd,
                        ..self.session_metadata(&session_id)?
                    },
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

    fn session_toolset_config(&self, session_config: &SessionConfig) -> ToolsetConfig {
        let mut config = ToolsetConfig::empty();
        config.host = match effective_host_tool_mode(session_config) {
            HostToolMode::None => tools::toolset::HostToolsetConfig::disabled(),
            HostToolMode::ReadOnly => tools::toolset::HostToolsetConfig {
                fs: tools::toolset::HostFsToolsetConfig::read_only(),
                ..tools::toolset::HostToolsetConfig::disabled()
            },
            HostToolMode::Edit => tools::toolset::HostToolsetConfig::workspace(),
        };
        if effective_web_search_enabled(session_config) {
            config.openai_web_search = OpenAiResponsesWebSearchConfig::cached();
        }
        config
    }

    fn workflow_args(
        &self,
        session_id: SessionId,
        session_config: SessionConfig,
    ) -> AgentSessionArgs {
        AgentSessionArgs {
            session_id,
            session_config,
            instructions_ref: self.instructions_ref.clone(),
            max_steps_per_input: self.max_steps_per_input,
            continue_as_new_history_threshold: self.continue_as_new_history_threshold,
        }
    }

    fn projector(&self) -> CoreAgentProjector<'_> {
        CoreAgentProjector::new(self.store.as_ref())
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
        let mut session = self
            .projector()
            .project_session(ProjectSession {
                session_id,
                state: &loaded.state,
                record: &loaded.record,
                entries: &loaded.entries,
                cwd: metadata.cwd.clone(),
            })
            .await?;
        session.vfs_mounts = self.project_vfs_mounts(session_id).await?;
        Ok(session)
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
}

fn effective_web_search_enabled(session_config: &SessionConfig) -> bool {
    session_config.model.api_kind == ProviderApiKind::OpenAiResponses
        && session_config.tools.web_search.unwrap_or(true)
}

fn effective_host_tool_mode(session_config: &SessionConfig) -> HostToolMode {
    session_config.tools.host.unwrap_or(HostToolMode::Edit)
}

pub(super) struct LoadedSession {
    pub(super) record: engine::storage::SessionRecord,
    pub(super) entries: Vec<engine::CoreAgentEntry>,
    pub(super) state: engine::CoreAgentState,
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
        let session_config = self.session_config_for_start(config.clone()).await?;
        self.write_session_metadata(session_id.clone(), GatewaySessionMetadata { cwd })?;
        self.client
            .start_workflow(
                AgentSessionWorkflow::run,
                self.workflow_args(session_id.clone(), session_config),
                WorkflowStartOptions::new(self.task_queue.clone(), session_id.as_str()).build(),
            )
            .await
            .map_err(map_workflow_start_error)?;
        self.wait_for_open_session(&session_id).await?;
        let loaded = self.load_session_state(&session_id).await?;
        let session = self.configure_session_toolset(&session_id, &loaded).await?;
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
        self.wait_for_config_revision(&session_id, target_revision, baseline_failures)
            .await?;
        let loaded = self.load_session_state(&session_id).await?;
        let session = self.configure_session_toolset(&session_id, &loaded).await?;
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

    async fn compact_context(
        &self,
        params: ContextCompactParams,
    ) -> Result<AgentApiOutcome<ContextCompactResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let loaded = self.load_session_state(&session_id).await?;
        let baseline_revision = loaded.state.context.revision;
        let baseline_failures = self
            .query_status_optional(&session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(&session_id, CoreAgentCommand::CompactContext)
            .await?;
        let session = self
            .wait_for_context_compaction_complete(&session_id, baseline_revision, baseline_failures)
            .await?;
        Ok(AgentApiOutcome::new(ContextCompactResponse { session }))
    }

    async fn start_run(
        &self,
        params: RunStartParams,
    ) -> Result<AgentApiOutcome<RunStartResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        self.load_session_state_with_current_run_context(&session_id)
            .await?;
        let submission_id = self.allocate_submission_id();
        let run_config = self
            .run_config_for_start(&session_id, params.config)
            .await?;
        let input = run_input_from_api(self.store.as_ref(), &params.input).await?;
        let command = engine::CoreAgentCodec
            .encode_command(&CoreAgentCommand::RequestRun {
                submission_id: Some(submission_id.clone()),
                input,
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

    async fn active_prompts(
        &self,
        params: PromptsActiveParams,
    ) -> Result<AgentApiOutcome<PromptsActiveResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let loaded = self.load_session_state(&session_id).await?;
        Ok(AgentApiOutcome::new(
            self.project_active_prompts(&loaded).await?,
        ))
    }

    async fn list_skills(
        &self,
        params: SkillListParams,
    ) -> Result<AgentApiOutcome<SkillListResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let loaded = self
            .load_session_state_with_current_skill_catalog(&session_id)
            .await?;
        Ok(AgentApiOutcome::new(
            self.project_skill_list(&loaded).await?,
        ))
    }

    async fn active_skills(
        &self,
        params: SkillActiveParams,
    ) -> Result<AgentApiOutcome<SkillActiveResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let loaded = self
            .load_session_state_with_current_skill_catalog(&session_id)
            .await?;
        Ok(AgentApiOutcome::new(
            self.project_active_skills(&loaded).await?,
        ))
    }

    async fn activate_skill(
        &self,
        params: SkillActivateParams,
    ) -> Result<AgentApiOutcome<SkillActivateResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let skill_id = SkillId::try_new(params.skill_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid skill id: {error}"))
        })?;
        let loaded = self
            .load_session_state_with_current_skill_catalog(&session_id)
            .await?;
        self.require_open_idle_session(&session_id, &loaded, "skill activation")?;

        let catalog_ref = active_skill_catalog_ref(&loaded.state).ok_or_else(|| {
            AgentApiError::not_found(format!("no skill catalog is available for {session_id}"))
        })?;
        let catalog = self.read_skill_catalog(&catalog_ref).await?;
        let skill = catalog
            .skills
            .iter()
            .find(|skill| skill.skill_id == skill_id)
            .ok_or_else(|| AgentApiError::not_found(format!("skill not found: {skill_id}")))?;
        if !skill.enabled {
            return Err(AgentApiError::rejected(format!(
                "skill is disabled: {skill_id}"
            )));
        }

        let skill_doc = self
            .read_skill_doc_for_activation(&session_id, skill)
            .await?;
        let context_ref = self
            .store
            .put_bytes(skill_doc.into_bytes())
            .await
            .map_err(map_blob_store_error)?;
        let entry = skill_activation_context_input(
            skill_id.clone(),
            catalog_ref.clone(),
            context_ref.clone(),
            params.scope,
            Some(skill),
        );
        let target_active_ids = active_skill_ids_after_upsert(&loaded.state, skill_id.clone());
        let baseline_failures = self
            .query_status_optional(&session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(
            &session_id,
            CoreAgentCommand::UpsertContext {
                key: skill_activation_context_key(&skill_id),
                entry,
            },
        )
        .await?;
        self.wait_for_skill_activations(&session_id, target_active_ids, baseline_failures)
            .await?;

        let loaded = self.load_session_state(&session_id).await?;
        let active = self.project_active_skills(&loaded).await?.activations;
        let activation = active
            .iter()
            .find(|active| active.skill_id == skill_id.as_str())
            .cloned()
            .unwrap_or_else(|| SkillActivationView {
                skill_id: skill_id.as_str().to_owned(),
                name: Some(skill.name.clone()),
                description: Some(skill.description.clone()),
                short_description: skill.short_description.clone(),
                catalog_ref: catalog_ref.as_str().to_owned(),
                scope: params.scope,
                source: ApiSkillActivationSource::DirectContext {
                    context_ref: context_ref.as_str().to_owned(),
                },
            });
        Ok(AgentApiOutcome::new(SkillActivateResponse {
            activation,
            active,
        }))
    }

    async fn deactivate_skill(
        &self,
        params: SkillDeactivateParams,
    ) -> Result<AgentApiOutcome<SkillDeactivateResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let skill_id = SkillId::try_new(params.skill_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid skill id: {error}"))
        })?;
        let loaded = self.load_session_state(&session_id).await?;
        self.require_open_idle_session(&session_id, &loaded, "skill deactivation")?;

        if !active_skill_ids(&loaded.state).contains(&skill_id) {
            return Err(AgentApiError::not_found(format!(
                "active skill not found: {skill_id}"
            )));
        }
        let target_active_ids = active_skill_ids_after_remove(&loaded.state, &skill_id);

        let baseline_failures = self
            .query_status_optional(&session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(
            &session_id,
            CoreAgentCommand::RemoveContext {
                key: skill_activation_context_key(&skill_id),
            },
        )
        .await?;
        self.wait_for_skill_activations(&session_id, target_active_ids, baseline_failures)
            .await?;

        let loaded = self.load_session_state(&session_id).await?;
        let active = self.project_active_skills(&loaded).await?.activations;
        Ok(AgentApiOutcome::new(SkillDeactivateResponse {
            skill_id: skill_id.as_str().to_owned(),
            active,
        }))
    }

    async fn put_blob(
        &self,
        params: BlobPutParams,
    ) -> Result<AgentApiOutcome<BlobPutResponse>, AgentApiError> {
        put_blob(self.store.as_ref(), params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn put_blobs(
        &self,
        params: BlobPutManyParams,
    ) -> Result<AgentApiOutcome<BlobPutManyResponse>, AgentApiError> {
        put_blobs(self.store.as_ref(), params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn get_blob(
        &self,
        params: BlobGetParams,
    ) -> Result<AgentApiOutcome<BlobGetResponse>, AgentApiError> {
        get_blob(self.store.as_ref(), params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn has_blobs(
        &self,
        params: BlobHasManyParams,
    ) -> Result<AgentApiOutcome<BlobHasManyResponse>, AgentApiError> {
        has_blobs(self.store.as_ref(), params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn commit_vfs_snapshot(
        &self,
        params: VfsSnapshotCommitParams,
    ) -> Result<AgentApiOutcome<VfsSnapshotCommitResponse>, AgentApiError> {
        let response = commit_vfs_snapshot(self.store.as_ref(), params).await?;
        let snapshot_ref = parse_blob_ref(&response.snapshot_ref)?;
        self.record_vfs_snapshot(
            snapshot_ref,
            VfsSnapshotSource::new("api_commit").with_subject("vfs/snapshot/commit"),
            None,
        )
        .await?;
        Ok(AgentApiOutcome::new(response))
    }

    async fn read_vfs_snapshot(
        &self,
        params: VfsSnapshotReadParams,
    ) -> Result<AgentApiOutcome<VfsSnapshotReadResponse>, AgentApiError> {
        read_vfs_snapshot(self.store.as_ref(), params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn create_vfs_workspace(
        &self,
        params: VfsWorkspaceCreateParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceCreateResponse>, AgentApiError> {
        let workspace = self.create_vfs_workspace_record(params).await?;
        Ok(AgentApiOutcome::new(VfsWorkspaceCreateResponse {
            workspace: vfs_workspace_view(workspace),
        }))
    }

    async fn read_vfs_workspace(
        &self,
        params: VfsWorkspaceReadParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceReadResponse>, AgentApiError> {
        let workspace = self.read_vfs_workspace_record(params).await?;
        Ok(AgentApiOutcome::new(VfsWorkspaceReadResponse {
            workspace: vfs_workspace_view(workspace),
        }))
    }

    async fn update_vfs_workspace(
        &self,
        params: VfsWorkspaceUpdateParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceUpdateResponse>, AgentApiError> {
        let workspace = self.update_vfs_workspace_record(params).await?;
        Ok(AgentApiOutcome::new(VfsWorkspaceUpdateResponse {
            workspace: vfs_workspace_view(workspace),
        }))
    }

    async fn delete_vfs_workspace(
        &self,
        params: VfsWorkspaceDeleteParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceDeleteResponse>, AgentApiError> {
        let workspace = self.delete_vfs_workspace_record(params).await?;
        Ok(AgentApiOutcome::new(VfsWorkspaceDeleteResponse {
            workspace: vfs_workspace_view(workspace),
        }))
    }

    async fn put_vfs_mount(
        &self,
        params: VfsMountPutParams,
    ) -> Result<AgentApiOutcome<VfsMountPutResponse>, AgentApiError> {
        let (mount, session) = self.put_vfs_mount_record(params).await?;
        Ok(AgentApiOutcome::new(VfsMountPutResponse {
            mount: self.vfs_mount_view(mount).await?,
            session,
        }))
    }

    async fn delete_vfs_mount(
        &self,
        params: VfsMountDeleteParams,
    ) -> Result<AgentApiOutcome<VfsMountDeleteResponse>, AgentApiError> {
        let (mount_path, session) = self.delete_vfs_mount_record(params).await?;
        Ok(AgentApiOutcome::new(VfsMountDeleteResponse {
            mount_path,
            session,
        }))
    }

    async fn list_vfs_mounts(
        &self,
        params: VfsMountListParams,
    ) -> Result<AgentApiOutcome<VfsMountListResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        self.store
            .load_session(&session_id)
            .await
            .map_err(map_session_store_error)?
            .ok_or_else(|| AgentApiError::not_found(format!("session not found: {session_id}")))?;
        Ok(AgentApiOutcome::new(VfsMountListResponse {
            mounts: self.project_vfs_mounts(&session_id).await?,
        }))
    }
}
#[cfg(test)]
mod tests;
