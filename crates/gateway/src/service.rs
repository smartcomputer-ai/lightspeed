//! `api` gateway for the Temporal-backed agent workflow.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    sync::{Arc, RwLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use api::{
    AgentApiError, AgentApiErrorKind, AgentApiOutcome, AgentApiService, BlobGetParams,
    BlobGetResponse, BlobHasItem, BlobHasManyParams, BlobHasManyResponse, BlobPutManyParams,
    BlobPutManyResponse, BlobPutParams, BlobPutResponse, ClientCapabilities,
    ContextConfigInput as ApiContextConfigInput, ContextConfigPatchInput, FieldPatch,
    GenerationConfig, GenerationConfigPatch, InitializeParams, InitializeResponse, InputItem,
    InstructionsSource, ModelConfig, ReasoningEffort, RunCancelParams, RunCancelResponse,
    RunDefaultsConfig, RunDefaultsPatch, RunLimitsConfig, RunStartConfig, RunStartParams,
    RunStartResponse, RunView, ServerCapabilities, ServerInfo, SessionCloseParams,
    SessionCloseResponse, SessionConfigInput, SessionConfigPatchInput, SessionEventsReadParams,
    SessionEventsReadResponse, SessionReadParams, SessionReadResponse, SessionStartParams,
    SessionStartResponse, SessionUpdateParams, SessionUpdateResponse, SessionView,
    SkillActivateParams, SkillActivateResponse, SkillActivationScope as ApiSkillActivationScope,
    SkillActivationSource as ApiSkillActivationSource, SkillActivationView, SkillActiveParams,
    SkillActiveResponse, SkillDeactivateParams, SkillDeactivateResponse, SkillListItem,
    SkillListParams, SkillListResponse, VfsMountAccess as ApiVfsMountAccess, VfsMountDeleteParams,
    VfsMountDeleteResponse, VfsMountListParams, VfsMountListResponse, VfsMountPutParams,
    VfsMountPutResponse, VfsMountSourceInput, VfsMountSourceView, VfsMountView,
    VfsSnapshotCommitParams, VfsSnapshotCommitResponse, VfsSnapshotReadParams,
    VfsSnapshotReadResponse, VfsWorkspaceCreateParams, VfsWorkspaceCreateResponse,
    VfsWorkspaceDeleteParams, VfsWorkspaceDeleteResponse, VfsWorkspaceReadParams,
    VfsWorkspaceReadResponse, VfsWorkspaceUpdateParams, VfsWorkspaceUpdateResponse,
    VfsWorkspaceView,
};
use api_projection::{
    CoreAgentProjector, MAX_EVENT_PAGE_LIMIT, ProjectSession, api_kind_from_str, api_run_id,
    decode_dynamic_entry, event_cursor, event_page_limit, map_session_store_error,
    parse_api_run_id, read_all_session_entries, replay_core_agent_state,
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use engine::{
    AnthropicMessagesRequestDefaults, BlobRef, CommandCodec, ContextConfigPatch, ContextEntryInput,
    ContextEntryKind, ContextMessageRole, CoreAgentCommand, CoreAgentStatus, ModelProviderOptions,
    ModelSelection, OpenAiCompletionsRequestDefaults, OpenAiReasoningConfig,
    OpenAiResponsesRequestDefaults, OptionalConfigPatch, ProviderApiKind, ProviderRequestDefaults,
    RunConfig, RunConfigPatch, RunId, RunStatus, SessionConfig, SessionConfigPatch, SessionId,
    SkillActivation, SkillActivationScope, SkillActivationSource, SkillCatalogContext, SkillId,
    SubmissionId, TurnConfigPatch,
    storage::{BlobStore, BlobStoreError, ReadSessionEvents, SessionStore},
};
use store_pg::PgStore;
use temporalio_client::{
    Client, WorkflowHandle, WorkflowQueryOptions, WorkflowSignalOptions, WorkflowStartOptions,
    errors::WorkflowInteractionError, errors::WorkflowQueryError, errors::WorkflowStartError,
};
use tools::{
    host::{
        HostToolContext, HostToolTargets,
        fs::{FileSystem, FsPath, MountedVfsFileSystem},
        profiles::{HostToolPreset, resolve_host_profile},
    },
    runtime::ToolDocument,
    skills::{
        SkillCatalogSnapshot, SkillLocation, SkillMetadata, conventional_vfs_skill_root_specs,
        prepare_skill_catalog_publication, resolve_mounted_vfs_skill_roots,
    },
};
use vfs::{
    CompareAndSetVfsWorkspaceHead, CreateVfsWorkspaceRecord, VfsCatalogError, VfsMountAccess,
    VfsMountRecord, VfsMountSource, VfsMountStore, VfsPath, VfsSnapshotRecord, VfsSnapshotSource,
    VfsSnapshotStore, VfsWorkspaceId, VfsWorkspaceRecord, VfsWorkspaceStore,
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
        let mut session = self
            .projector()
            .project_session(ProjectSession {
                session_id,
                state: &loaded.state,
                record: &loaded.record,
                entries: &loaded.entries,
                cwd: metadata.cwd,
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

    async fn load_session_state_with_current_skill_catalog(
        &self,
        session_id: &SessionId,
    ) -> Result<LoadedSession, AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        if loaded.state.lifecycle.status == CoreAgentStatus::Open
            && loaded.state.runs.active.is_none()
            && loaded.state.runs.queued.is_empty()
        {
            self.refresh_skill_catalog_for_idle_session(
                session_id,
                loaded.state.skills.catalog.clone(),
            )
            .await?;
            return self.load_session_state(session_id).await;
        }
        Ok(loaded)
    }

    async fn refresh_skill_catalog_for_idle_session(
        &self,
        session_id: &SessionId,
        active_catalog: Option<SkillCatalogContext>,
    ) -> Result<(), AgentApiError> {
        let Some(command) = self
            .skill_catalog_refresh_command(session_id, active_catalog)
            .await?
        else {
            return Ok(());
        };
        let CoreAgentCommand::SetSkillCatalog { catalog } = &command else {
            return Err(AgentApiError::internal(
                "skill catalog refresh produced non-catalog command",
            ));
        };
        let target_catalog_ref = catalog.as_ref().map(|catalog| catalog.catalog_ref.clone());
        let baseline_failures = self
            .query_status_optional(session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(session_id, command).await?;
        self.wait_for_skill_catalog(session_id, target_catalog_ref, baseline_failures)
            .await
    }

    async fn skill_catalog_refresh_command(
        &self,
        session_id: &SessionId,
        active_catalog: Option<SkillCatalogContext>,
    ) -> Result<Option<CoreAgentCommand>, AgentApiError> {
        let mounts = self
            .store
            .list_mounts(session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        let specs = conventional_vfs_skill_root_specs(&mounts);
        if specs.is_empty() {
            return Ok(clear_skill_catalog_command(active_catalog.as_ref()));
        }

        let blobs: Arc<dyn BlobStore> = self.store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = self.store.clone();
        let resolved = resolve_mounted_vfs_skill_roots(blobs, workspace_store, mounts, specs)
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        let inputs = resolved
            .existing_directory_inputs()
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        if inputs.is_empty() {
            return Ok(clear_skill_catalog_command(active_catalog.as_ref()));
        }

        let mut state = engine::CoreAgentState::new();
        state.skills.catalog = active_catalog;
        let publication =
            prepare_skill_catalog_publication(self.store.as_ref(), &state, None, &inputs)
                .await
                .map_err(|error| AgentApiError::internal(error.to_string()))?;
        Ok(publication.command)
    }

    async fn project_skill_list(
        &self,
        loaded: &LoadedSession,
    ) -> Result<SkillListResponse, AgentApiError> {
        let Some(catalog_context) = loaded.state.skills.catalog.as_ref() else {
            return Ok(SkillListResponse {
                catalog_ref: None,
                skills: Vec::new(),
            });
        };
        let catalog = self
            .read_skill_catalog(&catalog_context.catalog_ref)
            .await?;
        Ok(skill_list_response(
            Some(&catalog_context.catalog_ref),
            Some(&catalog),
            &loaded.state.skills.activations,
        ))
    }

    async fn project_active_skills(
        &self,
        loaded: &LoadedSession,
    ) -> Result<SkillActiveResponse, AgentApiError> {
        let catalog = match loaded.state.skills.catalog.as_ref() {
            Some(catalog_context) => Some(
                self.read_skill_catalog(&catalog_context.catalog_ref)
                    .await?,
            ),
            None => None,
        };
        Ok(skill_active_response(
            loaded
                .state
                .skills
                .catalog
                .as_ref()
                .map(|catalog| &catalog.catalog_ref),
            catalog.as_ref(),
            &loaded.state.skills.activations,
        ))
    }

    async fn read_skill_catalog(
        &self,
        catalog_ref: &BlobRef,
    ) -> Result<SkillCatalogSnapshot, AgentApiError> {
        let bytes = self
            .store
            .read_bytes(catalog_ref)
            .await
            .map_err(map_blob_read_error)?;
        serde_json::from_slice(&bytes).map_err(|error| {
            AgentApiError::internal(format!("stored skill catalog is invalid JSON: {error}"))
        })
    }

    async fn read_skill_doc_for_activation(
        &self,
        session_id: &SessionId,
        skill: &SkillMetadata,
    ) -> Result<String, AgentApiError> {
        let mounts = self
            .store
            .list_mounts(session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        let blobs: Arc<dyn BlobStore> = self.store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = self.store.clone();
        read_skill_doc_for_activation_from_vfs(blobs, workspace_store, mounts, skill).await
    }

    fn require_open_idle_session(
        &self,
        session_id: &SessionId,
        loaded: &LoadedSession,
        operation: &str,
    ) -> Result<(), AgentApiError> {
        if loaded.state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        if loaded.state.runs.active.is_some() || !loaded.state.runs.queued.is_empty() {
            return Err(AgentApiError::rejected(format!(
                "{operation} can only change while no run is active or queued"
            )));
        }
        Ok(())
    }

    async fn wait_for_skill_catalog(
        &self,
        session_id: &SessionId,
        target_catalog_ref: Option<BlobRef>,
        baseline_failures: usize,
    ) -> Result<(), AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for skill catalog update: {session_id}"
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
            let actual = loaded
                .state
                .skills
                .catalog
                .as_ref()
                .map(|catalog| catalog.catalog_ref.clone());
            if actual == target_catalog_ref {
                return Ok(());
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    async fn wait_for_skill_activations(
        &self,
        session_id: &SessionId,
        target: Vec<SkillActivation>,
        baseline_failures: usize,
    ) -> Result<(), AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for skill activation update: {session_id}"
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
            if loaded.state.skills.activations == target {
                return Ok(());
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    async fn project_vfs_mounts(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<VfsMountView>, AgentApiError> {
        let mounts = self
            .store
            .list_mounts(session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        let mut views = Vec::with_capacity(mounts.len());
        for mount in mounts {
            views.push(self.vfs_mount_view(mount).await?);
        }
        Ok(views)
    }

    async fn vfs_mount_view(&self, mount: VfsMountRecord) -> Result<VfsMountView, AgentApiError> {
        Ok(VfsMountView {
            mount_path: mount.mount_path.as_str().to_owned(),
            source: match mount.source {
                VfsMountSource::Snapshot { snapshot_ref } => VfsMountSourceView::Snapshot {
                    snapshot_ref: snapshot_ref.as_str().to_owned(),
                },
                VfsMountSource::Workspace { workspace_id } => {
                    let workspace = self
                        .store
                        .read_workspace(&workspace_id)
                        .await
                        .map_err(map_vfs_catalog_error)?;
                    VfsMountSourceView::Workspace {
                        workspace_id: workspace.workspace_id.as_str().to_owned(),
                        head_snapshot_ref: Some(workspace.head_snapshot_ref.as_str().to_owned()),
                        revision: Some(workspace.revision),
                    }
                }
            },
            access: api_vfs_mount_access(mount.access),
        })
    }

    async fn create_vfs_workspace_record(
        &self,
        params: VfsWorkspaceCreateParams,
    ) -> Result<VfsWorkspaceRecord, AgentApiError> {
        let snapshot_ref = parse_blob_ref(&params.snapshot_ref)?;
        let _manifest = vfs::read_snapshot_manifest(self.store.as_ref(), &snapshot_ref)
            .await
            .map_err(map_vfs_read_error)?;
        self.record_vfs_snapshot_if_missing(
            snapshot_ref.clone(),
            VfsSnapshotSource::new("api_snapshot").with_subject("vfs/workspace/create"),
            params.display_name,
        )
        .await?;

        let workspace_id = match params.workspace_id {
            Some(workspace_id) => VfsWorkspaceId::try_new(workspace_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid vfs workspace id: {error}"))
            })?,
            None => self.allocate_vfs_workspace_id(),
        };
        self.store
            .create_workspace(CreateVfsWorkspaceRecord {
                workspace_id,
                base_snapshot_ref: Some(snapshot_ref.clone()),
                head_snapshot_ref: snapshot_ref,
                created_at_ms: now_ms()?,
            })
            .await
            .map_err(map_vfs_catalog_error)
    }

    async fn read_vfs_workspace_record(
        &self,
        params: VfsWorkspaceReadParams,
    ) -> Result<VfsWorkspaceRecord, AgentApiError> {
        let workspace_id = parse_vfs_workspace_id(params.workspace_id)?;
        self.store
            .read_workspace(&workspace_id)
            .await
            .map_err(map_vfs_catalog_error)
    }

    async fn update_vfs_workspace_record(
        &self,
        params: VfsWorkspaceUpdateParams,
    ) -> Result<VfsWorkspaceRecord, AgentApiError> {
        let workspace_id = parse_vfs_workspace_id(params.workspace_id)?;
        let snapshot_ref = parse_blob_ref(&params.snapshot_ref)?;
        vfs::read_snapshot_manifest(self.store.as_ref(), &snapshot_ref)
            .await
            .map_err(map_vfs_read_error)?;
        self.record_vfs_snapshot_if_missing(
            snapshot_ref.clone(),
            VfsSnapshotSource::new("api_workspace_update").with_subject("vfs/workspace/update"),
            params.display_name,
        )
        .await?;
        self.store
            .compare_and_set_head(CompareAndSetVfsWorkspaceHead {
                workspace_id,
                expected_revision: params.expected_revision,
                new_head_snapshot_ref: snapshot_ref,
                updated_at_ms: now_ms()?,
            })
            .await
            .map_err(map_vfs_catalog_error)
    }

    async fn delete_vfs_workspace_record(
        &self,
        params: VfsWorkspaceDeleteParams,
    ) -> Result<VfsWorkspaceRecord, AgentApiError> {
        let workspace_id = parse_vfs_workspace_id(params.workspace_id)?;
        self.store
            .delete_workspace(&workspace_id)
            .await
            .map_err(map_vfs_catalog_error)
    }

    async fn record_vfs_snapshot(
        &self,
        snapshot_ref: BlobRef,
        source: VfsSnapshotSource,
        display_name: Option<String>,
    ) -> Result<(), AgentApiError> {
        self.store
            .record_snapshot(VfsSnapshotRecord {
                snapshot_ref,
                source,
                display_name,
                created_at_ms: now_ms()?,
            })
            .await
            .map_err(map_vfs_catalog_error)
    }

    async fn record_vfs_snapshot_if_missing(
        &self,
        snapshot_ref: BlobRef,
        source: VfsSnapshotSource,
        display_name: Option<String>,
    ) -> Result<(), AgentApiError> {
        match self.store.read_snapshot(&snapshot_ref).await {
            Ok(_) => Ok(()),
            Err(VfsCatalogError::NotFound { .. }) => {
                self.record_vfs_snapshot(snapshot_ref, source, display_name)
                    .await
            }
            Err(error) => Err(map_vfs_catalog_error(error)),
        }
    }

    fn allocate_vfs_workspace_id(&self) -> VfsWorkspaceId {
        VfsWorkspaceId::new(format!("workspace_{}", uuid::Uuid::new_v4().simple()))
    }

    async fn put_vfs_mount_record(
        &self,
        params: VfsMountPutParams,
    ) -> Result<(VfsMountRecord, SessionView), AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let mount_path = VfsPath::parse(&params.mount_path).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid vfs mount path: {error}"))
        })?;
        let access = core_vfs_mount_access(params.access);
        let source = self
            .validate_vfs_mount_source(params.source, access)
            .await?;

        let loaded = self.load_session_state(&session_id).await?;
        if loaded.state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        if loaded.state.runs.active.is_some() || !loaded.state.runs.queued.is_empty() {
            return Err(AgentApiError::rejected(
                "vfs mounts can only change while no run is active or queued",
            ));
        }

        let record = VfsMountRecord {
            session_id: session_id.clone(),
            mount_path,
            source,
            access,
        };
        let mut candidate_mounts = self
            .store
            .list_mounts(&session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        candidate_mounts.retain(|mount| mount.mount_path != record.mount_path);
        candidate_mounts.push(record.clone());
        self.validate_vfs_mount_table(candidate_mounts.clone())?;

        self.store
            .put_mount(record.clone())
            .await
            .map_err(map_vfs_catalog_error)?;
        let session = self
            .configure_vfs_host_tools(&session_id, &loaded, candidate_mounts)
            .await?;
        Ok((record, session))
    }

    async fn delete_vfs_mount_record(
        &self,
        params: VfsMountDeleteParams,
    ) -> Result<(String, SessionView), AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let mount_path = VfsPath::parse(&params.mount_path).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid vfs mount path: {error}"))
        })?;

        let loaded = self.load_session_state(&session_id).await?;
        if loaded.state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        if loaded.state.runs.active.is_some() || !loaded.state.runs.queued.is_empty() {
            return Err(AgentApiError::rejected(
                "vfs mounts can only change while no run is active or queued",
            ));
        }

        let mut candidate_mounts = self
            .store
            .list_mounts(&session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        let original_len = candidate_mounts.len();
        candidate_mounts.retain(|mount| mount.mount_path != mount_path);
        if candidate_mounts.len() == original_len {
            return Err(AgentApiError::not_found(format!(
                "vfs catalog mount not found: {session_id}:{mount_path}"
            )));
        }

        self.validate_vfs_mount_table(candidate_mounts.clone())?;
        self.store
            .remove_mount(&session_id, &mount_path)
            .await
            .map_err(map_vfs_catalog_error)?;
        let session = self
            .configure_vfs_host_tools(&session_id, &loaded, candidate_mounts)
            .await?;
        Ok((mount_path.as_str().to_owned(), session))
    }

    async fn validate_vfs_mount_source(
        &self,
        source: VfsMountSourceInput,
        access: VfsMountAccess,
    ) -> Result<VfsMountSource, AgentApiError> {
        match source {
            VfsMountSourceInput::Snapshot { snapshot_ref } => {
                if access.is_writable() {
                    return Err(AgentApiError::invalid_request(
                        "snapshot vfs mounts must be read-only",
                    ));
                }
                let snapshot_ref = parse_blob_ref(&snapshot_ref)?;
                vfs::read_snapshot_manifest(self.store.as_ref(), &snapshot_ref)
                    .await
                    .map_err(map_vfs_read_error)?;
                self.record_vfs_snapshot_if_missing(
                    snapshot_ref.clone(),
                    VfsSnapshotSource::new("api_mount").with_subject("vfs/mount/put"),
                    None,
                )
                .await?;
                Ok(VfsMountSource::Snapshot { snapshot_ref })
            }
            VfsMountSourceInput::Workspace { workspace_id } => {
                let workspace_id = VfsWorkspaceId::try_new(workspace_id).map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid vfs workspace id: {error}"))
                })?;
                let workspace = self
                    .store
                    .read_workspace(&workspace_id)
                    .await
                    .map_err(map_vfs_catalog_error)?;
                vfs::read_snapshot_manifest(self.store.as_ref(), &workspace.head_snapshot_ref)
                    .await
                    .map_err(map_vfs_read_error)?;
                Ok(VfsMountSource::Workspace { workspace_id })
            }
        }
    }

    fn validate_vfs_mount_table(&self, mounts: Vec<VfsMountRecord>) -> Result<(), AgentApiError> {
        let blobs: Arc<dyn BlobStore> = self.store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = self.store.clone();
        MountedVfsFileSystem::new(blobs, workspace_store, mounts)
            .map(|_| ())
            .map_err(map_fs_error)
    }

    async fn configure_vfs_host_tools(
        &self,
        session_id: &SessionId,
        loaded: &LoadedSession,
        mounts: Vec<VfsMountRecord>,
    ) -> Result<SessionView, AgentApiError> {
        let session_config = loaded.state.lifecycle.config.as_ref().ok_or_else(|| {
            AgentApiError::invalid_request(format!("session is missing config: {session_id}"))
        })?;
        let blobs: Arc<dyn BlobStore> = self.store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = self.store.clone();
        let fs = MountedVfsFileSystem::new(blobs.clone(), workspace_store, mounts)
            .map_err(map_fs_error)?;
        let cwd = mounted_vfs_cwd(fs.mounts())?;
        let ctx = HostToolContext::new(Arc::new(fs), None, blobs.clone()).with_cwd(cwd);
        let target = tools::runtime::ToolTarget::from(&session_config.model);
        let profile = resolve_host_profile(&ctx, &target, HostToolPreset::DirectFs)
            .map_err(|error| AgentApiError::internal(format!("build vfs host tools: {error}")))?;
        store_tool_documents(blobs.as_ref(), &profile.documents).await?;

        let mut registry = loaded.state.tooling.registry.clone();
        for (tool_name, spec) in profile.registry.tools {
            registry.tools.insert(tool_name, spec);
        }
        for (profile_id, tool_profile) in profile.registry.profiles {
            registry.profiles.insert(profile_id, tool_profile);
        }

        let baseline_failures = self
            .query_status_optional(session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(session_id, CoreAgentCommand::SetToolRegistry { registry })
            .await?;
        self.submit_core_command(
            session_id,
            CoreAgentCommand::SetDefaultToolTarget {
                target: HostToolTargets::local_execution_target(),
            },
        )
        .await?;
        self.submit_core_command(
            session_id,
            CoreAgentCommand::SelectToolProfile {
                profile_id: profile.profile_id.clone(),
            },
        )
        .await?;
        self.wait_for_vfs_tooling(session_id, &profile.profile_id, baseline_failures)
            .await
    }

    async fn submit_core_command(
        &self,
        session_id: &SessionId,
        command: CoreAgentCommand,
    ) -> Result<(), AgentApiError> {
        let command = engine::CoreAgentCodec
            .encode_command(&command)
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        self.workflow_handle(session_id)
            .signal(
                AgentSessionWorkflow::submit_admission,
                AgentAdmission { command },
                WorkflowSignalOptions::default(),
            )
            .await
            .map_err(map_workflow_interaction_error)
    }

    async fn wait_for_vfs_tooling(
        &self,
        session_id: &SessionId,
        profile_id: &engine::ToolProfileId,
        baseline_failures: usize,
    ) -> Result<SessionView, AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for vfs host tools to configure: {session_id}"
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
            let selected = loaded.state.tooling.selected_profile_id.as_ref() == Some(profile_id);
            let target = loaded
                .state
                .tooling
                .routing
                .default_targets
                .get(tools::host::HOST_TARGET_NAMESPACE);
            if selected && target == Some(&HostToolTargets::local_execution_target()) {
                return self.project_session_by_id(session_id).await;
            }
            tokio::time::sleep(self.poll_interval).await;
        }
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

        let catalog_context = loaded.state.skills.catalog.as_ref().ok_or_else(|| {
            AgentApiError::not_found(format!("no skill catalog is available for {session_id}"))
        })?;
        let catalog = self
            .read_skill_catalog(&catalog_context.catalog_ref)
            .await?;
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
        let (activation, activations) = replace_direct_skill_activation(
            &loaded.state.skills.activations,
            skill_id.clone(),
            catalog_context.catalog_ref.clone(),
            context_ref,
            params.scope,
        );

        if activations != loaded.state.skills.activations {
            let baseline_failures = self
                .query_status_optional(&session_id)
                .await?
                .map(|status| status.admission_failures.len())
                .unwrap_or(0);
            self.submit_core_command(
                &session_id,
                CoreAgentCommand::SetSkillActivations {
                    activations: activations.clone(),
                },
            )
            .await?;
            self.wait_for_skill_activations(&session_id, activations, baseline_failures)
                .await?;
        }

        let loaded = self.load_session_state(&session_id).await?;
        let active = self.project_active_skills(&loaded).await?.activations;
        let activation = active
            .iter()
            .find(|active| active.skill_id == skill_id.as_str())
            .cloned()
            .unwrap_or_else(|| skill_activation_view(&activation, Some(&catalog)));
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

        let activations = remove_skill_activation(&loaded.state.skills.activations, &skill_id)?;

        let baseline_failures = self
            .query_status_optional(&session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(
            &session_id,
            CoreAgentCommand::SetSkillActivations {
                activations: activations.clone(),
            },
        )
        .await?;
        self.wait_for_skill_activations(&session_id, activations, baseline_failures)
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

async fn put_blob(
    store: &dyn BlobStore,
    params: BlobPutParams,
) -> Result<BlobPutResponse, AgentApiError> {
    let bytes = decode_base64(&params.bytes_base64, "bytesBase64")?;
    let byte_len = u64::try_from(bytes.len())
        .map_err(|_| AgentApiError::invalid_request("blob byte length does not fit in u64"))?;
    let blob_ref = store.put_bytes(bytes).await.map_err(map_blob_store_error)?;
    Ok(BlobPutResponse {
        blob_ref: blob_ref.as_str().to_owned(),
        bytes: byte_len,
    })
}

async fn put_blobs(
    store: &dyn BlobStore,
    params: BlobPutManyParams,
) -> Result<BlobPutManyResponse, AgentApiError> {
    let mut byte_lens = Vec::with_capacity(params.blobs.len());
    let mut blobs = Vec::with_capacity(params.blobs.len());
    for (index, blob) in params.blobs.into_iter().enumerate() {
        let bytes = decode_base64(&blob.bytes_base64, format!("blobs[{index}].bytesBase64"))?;
        byte_lens.push(u64::try_from(bytes.len()).map_err(|_| {
            AgentApiError::invalid_request(format!(
                "blobs[{index}] byte length does not fit in u64"
            ))
        })?);
        blobs.push(bytes);
    }
    let blob_refs = store.put_many(blobs).await.map_err(map_blob_store_error)?;
    Ok(BlobPutManyResponse {
        blobs: blob_refs
            .into_iter()
            .zip(byte_lens)
            .map(|(blob_ref, bytes)| BlobPutResponse {
                blob_ref: blob_ref.as_str().to_owned(),
                bytes,
            })
            .collect(),
    })
}

async fn get_blob(
    store: &dyn BlobStore,
    params: BlobGetParams,
) -> Result<BlobGetResponse, AgentApiError> {
    let blob_ref = parse_blob_ref(&params.blob_ref)?;
    let bytes = store
        .read_bytes(&blob_ref)
        .await
        .map_err(map_blob_read_error)?;
    let byte_len = u64::try_from(bytes.len())
        .map_err(|_| AgentApiError::internal("blob byte length does not fit in u64"))?;
    Ok(BlobGetResponse {
        blob_ref: blob_ref.as_str().to_owned(),
        bytes_base64: BASE64.encode(bytes),
        bytes: byte_len,
    })
}

async fn has_blobs(
    store: &dyn BlobStore,
    params: BlobHasManyParams,
) -> Result<BlobHasManyResponse, AgentApiError> {
    let mut blobs = Vec::with_capacity(params.blob_refs.len());
    for blob_ref in params.blob_refs {
        let blob_ref = parse_blob_ref(&blob_ref)?;
        let exists = store
            .has_blob(&blob_ref)
            .await
            .map_err(map_blob_store_error)?;
        blobs.push(BlobHasItem {
            blob_ref: blob_ref.as_str().to_owned(),
            exists,
        });
    }
    Ok(BlobHasManyResponse { blobs })
}

async fn commit_vfs_snapshot(
    store: &dyn BlobStore,
    params: VfsSnapshotCommitParams,
) -> Result<VfsSnapshotCommitResponse, AgentApiError> {
    let manifest: vfs::VfsSnapshotManifest =
        serde_json::from_value(params.manifest).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid vfs snapshot manifest: {error}"))
        })?;
    manifest
        .validate()
        .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
    validate_vfs_manifest_blob_refs(store, &manifest).await?;
    let totals = manifest.totals.clone();
    let result = vfs::commit_snapshot_manifest(store, manifest)
        .await
        .map_err(map_vfs_commit_error)?;
    Ok(VfsSnapshotCommitResponse {
        snapshot_ref: result.snapshot_ref.as_str().to_owned(),
        files: totals.files,
        bytes: totals.bytes,
    })
}

async fn read_vfs_snapshot(
    store: &dyn BlobStore,
    params: VfsSnapshotReadParams,
) -> Result<VfsSnapshotReadResponse, AgentApiError> {
    let snapshot_ref = parse_blob_ref(&params.snapshot_ref)?;
    let manifest = vfs::read_snapshot_manifest(store, &snapshot_ref)
        .await
        .map_err(map_vfs_read_error)?;
    let manifest_value = serde_json::to_value(&manifest)
        .map_err(|error| AgentApiError::internal(format!("failed to encode manifest: {error}")))?;
    Ok(VfsSnapshotReadResponse {
        snapshot_ref: snapshot_ref.as_str().to_owned(),
        files: manifest.totals.files,
        bytes: manifest.totals.bytes,
        manifest: manifest_value,
    })
}

fn vfs_workspace_view(record: VfsWorkspaceRecord) -> VfsWorkspaceView {
    VfsWorkspaceView {
        workspace_id: record.workspace_id.as_str().to_owned(),
        base_snapshot_ref: record
            .base_snapshot_ref
            .map(|blob_ref| blob_ref.as_str().to_owned()),
        head_snapshot_ref: record.head_snapshot_ref.as_str().to_owned(),
        revision: record.revision,
    }
}

fn api_vfs_mount_access(access: VfsMountAccess) -> ApiVfsMountAccess {
    match access {
        VfsMountAccess::ReadOnly => ApiVfsMountAccess::ReadOnly,
        VfsMountAccess::ReadWrite => ApiVfsMountAccess::ReadWrite,
    }
}

fn core_vfs_mount_access(access: ApiVfsMountAccess) -> VfsMountAccess {
    match access {
        ApiVfsMountAccess::ReadOnly => VfsMountAccess::ReadOnly,
        ApiVfsMountAccess::ReadWrite => VfsMountAccess::ReadWrite,
    }
}

fn mounted_vfs_cwd(mounts: &[VfsMountRecord]) -> Result<FsPath, AgentApiError> {
    let cwd = if mounts
        .iter()
        .any(|mount| mount.mount_path.as_str() == "/workspace")
    {
        "/workspace"
    } else {
        "/"
    };
    FsPath::new(cwd).map_err(|error| AgentApiError::internal(error.to_string()))
}

fn clear_skill_catalog_command(
    active_catalog: Option<&SkillCatalogContext>,
) -> Option<CoreAgentCommand> {
    active_catalog.map(|_| CoreAgentCommand::SetSkillCatalog { catalog: None })
}

fn skill_list_response(
    catalog_ref: Option<&BlobRef>,
    catalog: Option<&SkillCatalogSnapshot>,
    activations: &[SkillActivation],
) -> SkillListResponse {
    let Some(catalog) = catalog else {
        return SkillListResponse {
            catalog_ref: None,
            skills: Vec::new(),
        };
    };
    let active_ids = activations
        .iter()
        .map(|activation| activation.skill_id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    SkillListResponse {
        catalog_ref: catalog_ref.map(|catalog_ref| catalog_ref.as_str().to_owned()),
        skills: catalog
            .skills
            .iter()
            .map(|skill| SkillListItem {
                skill_id: skill.skill_id.as_str().to_owned(),
                name: skill.name.clone(),
                description: skill.description.clone(),
                short_description: skill.short_description.clone(),
                enabled: skill.enabled,
                active: active_ids.contains(skill.skill_id.as_str()),
            })
            .collect(),
    }
}

fn skill_active_response(
    catalog_ref: Option<&BlobRef>,
    catalog: Option<&SkillCatalogSnapshot>,
    activations: &[SkillActivation],
) -> SkillActiveResponse {
    SkillActiveResponse {
        catalog_ref: catalog_ref.map(|catalog_ref| catalog_ref.as_str().to_owned()),
        activations: activations
            .iter()
            .map(|activation| skill_activation_view(activation, catalog))
            .collect(),
    }
}

fn replace_direct_skill_activation(
    current: &[SkillActivation],
    skill_id: SkillId,
    catalog_ref: BlobRef,
    context_ref: BlobRef,
    scope: ApiSkillActivationScope,
) -> (SkillActivation, Vec<SkillActivation>) {
    let activation = SkillActivation {
        skill_id: skill_id.clone(),
        catalog_ref,
        source: SkillActivationSource::DirectContext { context_ref },
        scope: core_skill_activation_scope(scope),
    };
    let mut activations = current.to_vec();
    activations.retain(|active| active.skill_id != skill_id);
    activations.push(activation.clone());
    (activation, activations)
}

fn remove_skill_activation(
    current: &[SkillActivation],
    skill_id: &SkillId,
) -> Result<Vec<SkillActivation>, AgentApiError> {
    let mut activations = current.to_vec();
    let original_len = activations.len();
    activations.retain(|active| &active.skill_id != skill_id);
    if activations.len() == original_len {
        return Err(AgentApiError::not_found(format!(
            "active skill not found: {skill_id}"
        )));
    }
    Ok(activations)
}

async fn read_skill_doc_for_activation_from_vfs(
    blobs: Arc<dyn BlobStore>,
    workspace_store: Arc<dyn VfsWorkspaceStore>,
    mounts: Vec<VfsMountRecord>,
    skill: &SkillMetadata,
) -> Result<String, AgentApiError> {
    let skill_doc_path = match &skill.location {
        SkillLocation::MountedSnapshot { skill_doc_path, .. }
        | SkillLocation::MountedWorkspace { skill_doc_path, .. } => skill_doc_path,
        SkillLocation::HostFilesystem { .. } => {
            return Err(AgentApiError::invalid_request(
                "direct skill activation currently supports VFS-mounted skills only",
            ));
        }
    };

    let fs = MountedVfsFileSystem::new(blobs, workspace_store, mounts).map_err(map_fs_error)?;
    let path = FsPath::new(skill_doc_path.as_str()).map_err(|error| {
        AgentApiError::internal(format!(
            "stored skill document path is invalid: {skill_doc_path}: {error}"
        ))
    })?;
    fs.read_file_text(&path).await.map_err(map_fs_error)
}

fn core_skill_activation_scope(scope: ApiSkillActivationScope) -> SkillActivationScope {
    match scope {
        ApiSkillActivationScope::Run => SkillActivationScope::Run,
        ApiSkillActivationScope::Session => SkillActivationScope::Session,
    }
}

fn api_skill_activation_scope(scope: SkillActivationScope) -> ApiSkillActivationScope {
    match scope {
        SkillActivationScope::Run => ApiSkillActivationScope::Run,
        SkillActivationScope::Session => ApiSkillActivationScope::Session,
    }
}

fn skill_activation_view(
    activation: &SkillActivation,
    catalog: Option<&SkillCatalogSnapshot>,
) -> SkillActivationView {
    let metadata = catalog.and_then(|catalog| {
        catalog
            .skills
            .iter()
            .find(|skill| skill.skill_id == activation.skill_id)
    });
    SkillActivationView {
        skill_id: activation.skill_id.as_str().to_owned(),
        name: metadata.map(|skill| skill.name.clone()),
        description: metadata.map(|skill| skill.description.clone()),
        short_description: metadata.and_then(|skill| skill.short_description.clone()),
        catalog_ref: activation.catalog_ref.as_str().to_owned(),
        scope: api_skill_activation_scope(activation.scope),
        source: match &activation.source {
            SkillActivationSource::ToolResult { call_id } => ApiSkillActivationSource::ToolResult {
                call_id: call_id.as_str().to_owned(),
            },
            SkillActivationSource::DirectContext { context_ref } => {
                ApiSkillActivationSource::DirectContext {
                    context_ref: context_ref.as_str().to_owned(),
                }
            }
        },
    }
}

async fn store_tool_documents(
    blobs: &dyn BlobStore,
    documents: &[ToolDocument],
) -> Result<(), AgentApiError> {
    for document in documents {
        let blob_ref = blobs
            .put_bytes(document.blob_bytes())
            .await
            .map_err(map_blob_store_error)?;
        if blob_ref != document.blob_ref {
            return Err(AgentApiError::internal(format!(
                "tool document blob ref mismatch: expected {}, got {}",
                document.blob_ref, blob_ref
            )));
        }
    }
    Ok(())
}

async fn run_input_from_api(
    store: &dyn BlobStore,
    input: &[InputItem],
) -> Result<Vec<ContextEntryInput>, AgentApiError> {
    let mut entries = Vec::new();
    for item in input {
        match item {
            InputItem::Text { text } => {
                let text = text.trim();
                if !text.is_empty() {
                    let content_ref = store
                        .put_bytes(text.as_bytes().to_vec())
                        .await
                        .map_err(map_blob_store_error)?;
                    entries.push(user_message_input(content_ref));
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
                    entries.push(user_message_input(blob_ref));
                }
            }
        }
    }

    if entries.is_empty() {
        return Err(empty_run_input_error());
    }
    Ok(entries)
}

fn user_message_input(content_ref: BlobRef) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content_ref,
        media_type: Some("text/plain".to_owned()),
        preview: None,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }
}

fn parse_blob_ref(value: &str) -> Result<BlobRef, AgentApiError> {
    BlobRef::parse(value).map_err(|error| AgentApiError::invalid_request(error.to_string()))
}

fn parse_vfs_workspace_id(value: String) -> Result<VfsWorkspaceId, AgentApiError> {
    VfsWorkspaceId::try_new(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid vfs workspace id: {error}"))
    })
}

fn decode_base64(value: &str, field: impl AsRef<str>) -> Result<Vec<u8>, AgentApiError> {
    BASE64.decode(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid base64 in {}: {error}", field.as_ref()))
    })
}

async fn validate_vfs_manifest_blob_refs(
    store: &dyn BlobStore,
    manifest: &vfs::VfsSnapshotManifest,
) -> Result<(), AgentApiError> {
    let mut refs = BTreeMap::new();
    collect_vfs_manifest_blob_refs(&manifest.root, &mut refs)?;
    for (blob_ref, expected_bytes) in refs {
        let info = store
            .stat_blob(&blob_ref)
            .await
            .map_err(map_vfs_manifest_blob_error)?;
        if info.byte_len != expected_bytes {
            return Err(AgentApiError::invalid_request(format!(
                "vfs manifest file size for {blob_ref} is {expected_bytes}, but stored blob size is {}",
                info.byte_len
            )));
        }
    }
    Ok(())
}

fn collect_vfs_manifest_blob_refs(
    directory: &vfs::VfsDirectory,
    refs: &mut BTreeMap<BlobRef, u64>,
) -> Result<(), AgentApiError> {
    for entry in directory.entries.values() {
        match entry {
            vfs::VfsEntry::File(file) => {
                if let Some(existing) = refs.insert(file.blob_ref.clone(), file.size_bytes)
                    && existing != file.size_bytes
                {
                    return Err(AgentApiError::invalid_request(format!(
                        "vfs manifest references blob {} with conflicting sizes: {existing} and {}",
                        file.blob_ref, file.size_bytes
                    )));
                }
            }
            vfs::VfsEntry::Directory(directory) => {
                collect_vfs_manifest_blob_refs(directory, refs)?;
            }
        }
    }
    Ok(())
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

fn now_ms() -> Result<i64, AgentApiError> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| AgentApiError::internal(format!("system clock is before epoch: {error}")))?
        .as_millis();
    i64::try_from(ms)
        .map_err(|_| AgentApiError::internal("current timestamp does not fit in i64 milliseconds"))
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

fn map_blob_read_error(error: BlobStoreError) -> AgentApiError {
    match error {
        BlobStoreError::NotFound { blob_ref } => {
            AgentApiError::not_found(format!("blob not found: {blob_ref}"))
        }
        BlobStoreError::Store { message } => AgentApiError::internal(message),
    }
}

fn map_vfs_manifest_blob_error(error: BlobStoreError) -> AgentApiError {
    match error {
        BlobStoreError::NotFound { blob_ref } => AgentApiError::invalid_request(format!(
            "vfs manifest references missing blob: {blob_ref}"
        )),
        BlobStoreError::Store { message } => AgentApiError::internal(message),
    }
}

fn map_vfs_commit_error(error: vfs::VfsError) -> AgentApiError {
    match error {
        vfs::VfsError::BlobStore(error) => map_blob_store_error(error),
        error => AgentApiError::invalid_request(error.to_string()),
    }
}

fn map_vfs_read_error(error: vfs::VfsError) -> AgentApiError {
    match error {
        vfs::VfsError::BlobStore(error) => map_blob_read_error(error),
        error => AgentApiError::invalid_request(error.to_string()),
    }
}

fn map_vfs_catalog_error(error: VfsCatalogError) -> AgentApiError {
    match error {
        VfsCatalogError::AlreadyExists { kind, id } => {
            AgentApiError::conflict(format!("vfs catalog {kind} already exists: {id}"))
        }
        VfsCatalogError::NotFound { kind, id } => {
            AgentApiError::not_found(format!("vfs catalog {kind} not found: {id}"))
        }
        VfsCatalogError::RevisionConflict { .. } => AgentApiError::conflict(error.to_string()),
        VfsCatalogError::InvalidInput { message } => AgentApiError::invalid_request(message),
        VfsCatalogError::Store { message } => AgentApiError::internal(message),
    }
}

fn map_fs_error(error: tools::host::fs::FsError) -> AgentApiError {
    match error {
        tools::host::fs::FsError::InvalidPath(error) => {
            AgentApiError::invalid_request(error.to_string())
        }
        tools::host::fs::FsError::InvalidInput { message } => {
            AgentApiError::invalid_request(message)
        }
        tools::host::fs::FsError::NotFound { path } => {
            AgentApiError::not_found(format!("vfs path not found: {path}"))
        }
        tools::host::fs::FsError::AlreadyExists { path } => {
            AgentApiError::conflict(format!("vfs path already exists: {path}"))
        }
        tools::host::fs::FsError::PermissionDenied { path } => {
            AgentApiError::rejected(format!("vfs permission denied: {path}"))
        }
        tools::host::fs::FsError::Unsupported { message }
        | tools::host::fs::FsError::InvalidData { message }
        | tools::host::fs::FsError::Failed { message } => AgentApiError::internal(message),
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
    use async_trait::async_trait;

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
    fn skill_list_response_marks_active_catalog_entries() {
        let catalog_ref = BlobRef::from_bytes(b"catalog");
        let catalog = test_skill_catalog(
            &catalog_ref,
            vec![
                test_skill_metadata("skill:review", "review", true),
                test_skill_metadata("skill:deploy", "deploy", false),
            ],
        );
        let activation = direct_activation(
            "skill:review",
            &catalog_ref,
            &BlobRef::from_bytes(b"review-body"),
            SkillActivationScope::Run,
        );

        let response = skill_list_response(Some(&catalog_ref), Some(&catalog), &[activation]);

        assert_eq!(response.catalog_ref.as_deref(), Some(catalog_ref.as_str()));
        assert_eq!(response.skills.len(), 2);
        assert_eq!(response.skills[0].skill_id, "skill:review");
        assert!(response.skills[0].enabled);
        assert!(response.skills[0].active);
        assert_eq!(response.skills[1].skill_id, "skill:deploy");
        assert!(!response.skills[1].enabled);
        assert!(!response.skills[1].active);
    }

    #[test]
    fn skill_active_response_exposes_activation_sources_and_metadata() {
        let catalog_ref = BlobRef::from_bytes(b"catalog");
        let context_ref = BlobRef::from_bytes(b"direct-body");
        let catalog = test_skill_catalog(
            &catalog_ref,
            vec![
                test_skill_metadata("skill:review", "review", true),
                test_skill_metadata("skill:deploy", "deploy", true),
            ],
        );
        let direct = direct_activation(
            "skill:review",
            &catalog_ref,
            &context_ref,
            SkillActivationScope::Session,
        );
        let tool = SkillActivation {
            skill_id: SkillId::new("skill:deploy"),
            catalog_ref: catalog_ref.clone(),
            source: SkillActivationSource::ToolResult {
                call_id: engine::ToolCallId::new("call_1"),
            },
            scope: SkillActivationScope::Run,
        };

        let response = skill_active_response(Some(&catalog_ref), Some(&catalog), &[direct, tool]);

        assert_eq!(response.catalog_ref.as_deref(), Some(catalog_ref.as_str()));
        assert_eq!(response.activations.len(), 2);
        assert_eq!(response.activations[0].name.as_deref(), Some("review"));
        assert_eq!(
            response.activations[0].source,
            ApiSkillActivationSource::DirectContext {
                context_ref: context_ref.as_str().to_owned()
            }
        );
        assert_eq!(
            response.activations[0].scope,
            ApiSkillActivationScope::Session
        );
        assert_eq!(response.activations[1].name.as_deref(), Some("deploy"));
        assert_eq!(
            response.activations[1].source,
            ApiSkillActivationSource::ToolResult {
                call_id: "call_1".to_owned()
            }
        );
    }

    #[test]
    fn replace_direct_skill_activation_replaces_same_skill_only() {
        let catalog_ref = BlobRef::from_bytes(b"catalog");
        let old_context_ref = BlobRef::from_bytes(b"old-body");
        let new_context_ref = BlobRef::from_bytes(b"new-body");
        let other = direct_activation(
            "skill:deploy",
            &catalog_ref,
            &BlobRef::from_bytes(b"deploy-body"),
            SkillActivationScope::Run,
        );
        let current = vec![
            direct_activation(
                "skill:review",
                &catalog_ref,
                &old_context_ref,
                SkillActivationScope::Run,
            ),
            other.clone(),
        ];

        let (activation, activations) = replace_direct_skill_activation(
            &current,
            SkillId::new("skill:review"),
            catalog_ref.clone(),
            new_context_ref.clone(),
            ApiSkillActivationScope::Session,
        );

        assert_eq!(activation.skill_id, SkillId::new("skill:review"));
        assert_eq!(activation.scope, SkillActivationScope::Session);
        assert_eq!(activation.direct_context_ref(), Some(&new_context_ref));
        assert_eq!(activations.len(), 2);
        assert_eq!(activations[0], other);
        assert_eq!(activations[1], activation);
        assert_eq!(
            activations
                .iter()
                .filter(|activation| activation.skill_id == SkillId::new("skill:review"))
                .count(),
            1
        );
    }

    #[test]
    fn remove_skill_activation_removes_selected_or_errors() {
        let catalog_ref = BlobRef::from_bytes(b"catalog");
        let review = direct_activation(
            "skill:review",
            &catalog_ref,
            &BlobRef::from_bytes(b"review-body"),
            SkillActivationScope::Run,
        );
        let deploy = direct_activation(
            "skill:deploy",
            &catalog_ref,
            &BlobRef::from_bytes(b"deploy-body"),
            SkillActivationScope::Session,
        );

        let remaining =
            remove_skill_activation(&[review, deploy.clone()], &SkillId::new("skill:review"))
                .expect("remove review");

        assert_eq!(remaining, vec![deploy]);
        let error = remove_skill_activation(&remaining, &SkillId::new("skill:missing"))
            .expect_err("missing skill should fail");
        assert_eq!(error.kind, AgentApiErrorKind::NotFound);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_skill_doc_for_activation_reads_cataloged_vfs_bytes() {
        let blobs = Arc::new(engine::storage::InMemoryBlobStore::new());
        let skill_body =
            "---\nname: review\ndescription: Use when testing review.\n---\nsecret body\n";
        let snapshot = vfs::create_inline_snapshot(
            blobs.as_ref(),
            vfs::CreateInlineSnapshotRequest::new(vec![
                vfs::InlineFile::new("review/SKILL.md", skill_body.as_bytes().to_vec()).unwrap(),
            ]),
        )
        .await
        .expect("create skill snapshot");
        let workspace_store = Arc::new(EmptyWorkspaceStore);
        let mount = VfsMountRecord {
            session_id: SessionId::new("session_1"),
            mount_path: VfsPath::parse("/skills/system").unwrap(),
            source: VfsMountSource::Snapshot {
                snapshot_ref: snapshot.snapshot_ref.clone(),
            },
            access: VfsMountAccess::ReadOnly,
        };
        let skill = test_skill_metadata_with_snapshot(
            "skill:review",
            "review",
            true,
            snapshot.snapshot_ref.clone(),
        );

        let body =
            read_skill_doc_for_activation_from_vfs(blobs, workspace_store, vec![mount], &skill)
                .await
                .expect("read skill doc");

        assert_eq!(body, skill_body);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_skill_doc_for_activation_rejects_host_locations() {
        let blobs = Arc::new(engine::storage::InMemoryBlobStore::new());
        let workspace_store = Arc::new(EmptyWorkspaceStore);
        let mut skill = test_skill_metadata("skill:host", "host", true);
        skill.location = SkillLocation::HostFilesystem {
            target: engine::ToolExecutionTarget::new("host", "vm-1"),
            root_path: "/skills".to_owned(),
            skill_dir_path: "/skills/host".to_owned(),
            skill_doc_path: "/skills/host/SKILL.md".to_owned(),
        };

        let error =
            read_skill_doc_for_activation_from_vfs(blobs, workspace_store, Vec::new(), &skill)
                .await
                .expect_err("host location should not read through VFS");

        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
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
    async fn run_input_from_api_preserves_single_text_ref() {
        let store = engine::storage::InMemoryBlobStore::new();
        let blob_ref = store.insert_text("hello from cas").await;

        let input = run_input_from_api(
            &store,
            &[InputItem::TextRef {
                blob_ref: blob_ref.as_str().to_owned(),
            }],
        )
        .await
        .expect("input");

        assert_eq!(input.len(), 1);
        assert_eq!(input[0].content_ref, blob_ref);
        assert_eq!(
            input[0].kind,
            engine::ContextEntryKind::Message {
                role: engine::ContextMessageRole::User,
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_input_from_api_stores_text_and_preserves_refs() {
        let store = engine::storage::InMemoryBlobStore::new();
        let blob_ref = store.insert_text(" second ").await;

        let input = run_input_from_api(
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
        .expect("input");

        assert_eq!(input.len(), 2);
        assert_ne!(input[0].content_ref, blob_ref);
        assert_eq!(input[1].content_ref, blob_ref);
        assert_eq!(
            store
                .read_text(&input[0].content_ref)
                .await
                .expect("stored input"),
            "first"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blob_api_helpers_put_get_and_check_many() {
        let store = engine::storage::InMemoryBlobStore::new();

        let put = put_blobs(
            &store,
            BlobPutManyParams {
                blobs: vec![
                    BlobPutParams {
                        bytes_base64: BASE64.encode(b"hello"),
                    },
                    BlobPutParams {
                        bytes_base64: BASE64.encode(b"world"),
                    },
                ],
            },
        )
        .await
        .expect("put blobs");
        assert_eq!(put.blobs.len(), 2);
        assert_eq!(put.blobs[0].bytes, 5);

        let has = has_blobs(
            &store,
            BlobHasManyParams {
                blob_refs: vec![
                    put.blobs[0].blob_ref.clone(),
                    BlobRef::from_bytes(b"missing").as_str().to_owned(),
                ],
            },
        )
        .await
        .expect("has blobs");
        assert_eq!(
            has.blobs.iter().map(|item| item.exists).collect::<Vec<_>>(),
            vec![true, false]
        );

        let read = get_blob(
            &store,
            BlobGetParams {
                blob_ref: put.blobs[1].blob_ref.clone(),
            },
        )
        .await
        .expect("get blob");
        assert_eq!(read.bytes_base64, BASE64.encode(b"world"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_snapshot_api_helpers_commit_and_read_manifest() {
        let store = engine::storage::InMemoryBlobStore::new();
        let snapshot = vfs::create_inline_snapshot(
            &store,
            vfs::CreateInlineSnapshotRequest::new(vec![
                vfs::InlineFile::new("README.md", b"hello\n".to_vec()).unwrap(),
            ]),
        )
        .await
        .expect("create snapshot");
        let manifest = serde_json::to_value(snapshot.manifest).expect("manifest json");

        let committed = commit_vfs_snapshot(
            &store,
            VfsSnapshotCommitParams {
                manifest: manifest.clone(),
            },
        )
        .await
        .expect("commit snapshot");
        assert_eq!(committed.files, 1);
        assert_eq!(committed.bytes, 6);

        let read = read_vfs_snapshot(
            &store,
            VfsSnapshotReadParams {
                snapshot_ref: committed.snapshot_ref,
            },
        )
        .await
        .expect("read snapshot");
        assert_eq!(read.manifest, manifest);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_snapshot_commit_rejects_missing_file_blob_refs() {
        let store = engine::storage::InMemoryBlobStore::new();
        let missing_ref = BlobRef::from_bytes(b"missing");
        let manifest = vfs::VfsSnapshotManifest {
            schema_version: vfs::VFS_SNAPSHOT_SCHEMA_VERSION.to_owned(),
            root: vfs::VfsDirectory {
                entries: BTreeMap::from([(
                    "missing.txt".to_owned(),
                    vfs::VfsEntry::File(vfs::VfsFile {
                        blob_ref: missing_ref,
                        size_bytes: 7,
                        media_type: None,
                        executable: false,
                    }),
                )]),
            },
            totals: vfs::VfsTotals { files: 1, bytes: 7 },
        };

        let error = commit_vfs_snapshot(
            &store,
            VfsSnapshotCommitParams {
                manifest: serde_json::to_value(manifest).expect("manifest json"),
            },
        )
        .await
        .expect_err("missing blob should fail");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
        assert!(error.message.contains("missing blob"));
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

    fn test_skill_catalog(
        _catalog_ref: &BlobRef,
        skills: Vec<SkillMetadata>,
    ) -> SkillCatalogSnapshot {
        SkillCatalogSnapshot::new(None, skills, Vec::new())
    }

    fn test_skill_metadata(skill_id: &str, name: &str, enabled: bool) -> SkillMetadata {
        let snapshot_ref = BlobRef::from_bytes(b"skills-snapshot");
        test_skill_metadata_with_snapshot(skill_id, name, enabled, snapshot_ref)
    }

    fn test_skill_metadata_with_snapshot(
        skill_id: &str,
        name: &str,
        enabled: bool,
        snapshot_ref: BlobRef,
    ) -> SkillMetadata {
        SkillMetadata {
            skill_id: SkillId::new(skill_id),
            name: name.to_owned(),
            description: format!("Use when testing {name}."),
            short_description: Some(format!("{name} skill")),
            source: tools::skills::SkillSource::Snapshot {
                root_id: "system".to_owned(),
                snapshot_ref: snapshot_ref.clone(),
            },
            scope: tools::skills::SkillScope::Global,
            target: None,
            enabled,
            trust: tools::skills::SkillTrustLevel::System,
            interface: None,
            dependencies: tools::skills::SkillDependencies::default(),
            location: SkillLocation::MountedSnapshot {
                source_snapshot_ref: snapshot_ref,
                source_mount_path: VfsPath::parse("/skills/system").unwrap(),
                skill_dir_path: VfsPath::parse(format!("/skills/system/{name}")).unwrap(),
                skill_doc_path: VfsPath::parse(format!("/skills/system/{name}/SKILL.md")).unwrap(),
            },
            skill_doc_ref: None,
        }
    }

    fn direct_activation(
        skill_id: &str,
        catalog_ref: &BlobRef,
        context_ref: &BlobRef,
        scope: SkillActivationScope,
    ) -> SkillActivation {
        SkillActivation {
            skill_id: SkillId::new(skill_id),
            catalog_ref: catalog_ref.clone(),
            source: SkillActivationSource::DirectContext {
                context_ref: context_ref.clone(),
            },
            scope,
        }
    }

    struct EmptyWorkspaceStore;

    #[async_trait]
    impl VfsWorkspaceStore for EmptyWorkspaceStore {
        async fn create_workspace(
            &self,
            _record: vfs::CreateVfsWorkspaceRecord,
        ) -> Result<vfs::VfsWorkspaceRecord, vfs::VfsCatalogError> {
            Err(workspace_not_found("create"))
        }

        async fn read_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<vfs::VfsWorkspaceRecord, vfs::VfsCatalogError> {
            Err(workspace_not_found(workspace_id.as_str()))
        }

        async fn compare_and_set_head(
            &self,
            _request: vfs::CompareAndSetVfsWorkspaceHead,
        ) -> Result<vfs::VfsWorkspaceRecord, vfs::VfsCatalogError> {
            Err(workspace_not_found("compare_and_set"))
        }

        async fn delete_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<vfs::VfsWorkspaceRecord, vfs::VfsCatalogError> {
            Err(workspace_not_found(workspace_id.as_str()))
        }
    }

    fn workspace_not_found(id: &str) -> vfs::VfsCatalogError {
        vfs::VfsCatalogError::NotFound {
            kind: "workspace",
            id: id.to_owned(),
        }
    }
}
