//! `api` gateway for the Temporal-backed agent workflow.

mod api_config;
mod auth_api;
mod blobs;
mod errors;
mod github_api;
mod input;
mod mcp_api;
mod oauth_api;
mod parse;
mod prompts;
mod skills;
mod tools_api;
mod vfs_api;
mod workflow;

#[cfg(test)]
use api_config::*;
use auth_api::{
    api_auth_provider_kind, auth_grant_import_draft, auth_grant_view, map_auth_registry_error,
    parse_auth_grant_id, registry_auth_grant_status_for_filter,
};
use github_api::{
    auth_provider_create_draft, auth_provider_view, github_installation_grant_draft,
    github_installation_view, map_github_app_error, parse_auth_provider_id,
};
use oauth_api::{
    auth_client_create_draft, auth_flow_view, cimd_config, map_mcp_oauth_error,
    mcp_oauth_target_from_record, oauth_client_view, oauth_redirect_uri, parse_auth_flow_id,
    parse_oauth_client_id,
};
use blobs::{get_blob, has_blobs, put_blob, put_blobs};
use errors::*;
use input::run_input_from_api;
use mcp_api::{
    apply_session_mcp_link, create_mcp_server_record, linked_session_mcp, map_mcp_registry_error,
    mcp_server_view, parse_mcp_server_id, parse_mcp_tool_name, remove_session_mcp_link,
    session_mcp_link_from_record,
};
use parse::*;
use skills::{
    active_skill_catalog_ref, active_skill_ids, active_skill_ids_after_remove,
    active_skill_ids_after_upsert, skill_activation_context_input,
};
#[cfg(test)]
use skills::{read_skill_doc_for_activation_from_vfs, skill_active_response, skill_list_response};
use vfs_api::{commit_vfs_snapshot, now_ms, read_vfs_snapshot, vfs_workspace_view};

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    sync::{Arc, RwLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use api::{
    AgentApiError, AgentApiErrorKind, AgentApiOutcome, AgentApiService, AuthClientCreateParams,
    AuthClientCreateResponse, AuthClientDeleteParams, AuthClientDeleteResponse,
    AuthClientListParams, AuthClientListResponse, AuthClientReadParams, AuthClientReadResponse,
    AuthFlowStartParams, AuthFlowStartResponse, AuthFlowStatusParams, AuthFlowStatusResponse,
    AuthGitHubInstallationGrantParams, AuthGitHubInstallationGrantResponse,
    AuthGitHubInstallationListParams, AuthGitHubInstallationListResponse,
    AuthProviderCreateParams, AuthProviderCreateResponse, AuthProviderDeleteParams,
    AuthProviderDeleteResponse, AuthProviderListParams, AuthProviderListResponse,
    AuthProviderReadParams, AuthProviderReadResponse,
    AuthGrantImportParams,
    AuthGrantImportResponse, AuthGrantListParams, AuthGrantListResponse, AuthGrantReadParams,
    AuthGrantReadResponse, AuthGrantRevokeParams, AuthGrantRevokeResponse, BlobGetParams,
    BlobGetResponse, BlobHasItem, BlobHasManyParams, BlobHasManyResponse, BlobPutManyParams,
    BlobPutManyResponse, BlobPutParams, BlobPutResponse, ClientCapabilities, CompactionPolicyInput,
    ContextCompactParams, ContextCompactResponse, ContextConfigInput as ApiContextConfigInput,
    ContextConfigPatchInput, FieldPatch, GenerationConfig, GenerationConfigPatch, InitializeParams,
    InitializeResponse, InputItem, McpServerCreateParams, McpServerCreateResponse,
    McpServerDeleteParams, McpServerDeleteResponse, McpServerListParams, McpServerListResponse,
    McpServerReadParams, McpServerReadResponse, ModelConfig, PromptInstructionView,
    PromptsActiveParams, PromptsActiveResponse, ReasoningEffort, RunCancelParams,
    RunCancelResponse, RunDefaultsConfig, RunDefaultsPatch, RunLimitsConfig, RunStartConfig,
    RunStartParams, RunStartResponse, RunView, ServerCapabilities, ServerInfo, SessionCloseParams,
    SessionCloseResponse, SessionConfigInput, SessionConfigPatchInput, SessionEventsReadParams,
    SessionEventsReadResponse, SessionMcpLinkParams, SessionMcpLinkResponse, SessionMcpListParams,
    SessionMcpListResponse, SessionMcpUnlinkParams, SessionMcpUnlinkResponse, SessionReadParams,
    SessionReadResponse, SessionStartParams, SessionStartResponse, SessionToolsUpdateParams,
    SessionToolsUpdateResponse, SessionUpdateParams, SessionUpdateResponse, SessionView,
    SkillActivateParams, SkillActivateResponse, SkillActivationScope as ApiSkillActivationScope,
    SkillActivationSource as ApiSkillActivationSource, SkillActivationView, SkillActiveParams,
    SkillActiveResponse, SkillDeactivateParams, SkillDeactivateResponse, SkillListItem,
    SkillListParams, SkillListResponse, ToolChoiceConfig, ToolChoiceModeConfig, ToolConfigInput,
    ToolConfigPatchInput, VfsMountAccess as ApiVfsMountAccess, VfsMountDeleteParams,
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
    BlobRef, CommandCodec, CompactionPolicy, ContextConfigPatch, ContextEntry, ContextEntryInput,
    ContextEntryKey, ContextEntryKind, ContextMessageRole, CoreAgentCommand, CoreAgentStatus,
    HostToolMode, ModelSelection, OptionalConfigPatch, ProviderApiKind,
    ProviderParams, RunConfig, RunConfigPatch, RunId, RunStatus,
    SKILL_ACTIVATION_PROVIDER_KIND_RUN, SKILL_ACTIVATION_PROVIDER_KIND_SESSION,
    SKILL_CATALOG_CONTEXT_KEY, SessionConfig, SessionConfigPatch, SessionId, SkillId, SubmissionId,
    ToolChoice, ToolChoiceMode, ToolName, TurnConfigPatch, skill_activation_context_key,
    storage::{BlobStore, BlobStoreError, ReadSessionEvents, SessionStore},
};
use auth_registry::{
    AuthFlowStore, AuthGrantStore, AuthProviderStore, GitHubApiClient, HttpGitHubApiClient,
    HttpOAuthMetadataClient, HttpOAuthTokenClient, McpOAuthDriver, OAuthClientStore,
    OAuthFlowService, OAuthMetadataClient, OAuthTokenClient, SecretStore, StartAuthFlow,
};
use mcp_registry::McpRegistryStore;
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
    web::fetch::WebFetchToolConfig,
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

/// Default public base URL for the gateway-hosted OAuth callback; matches
/// `DEFAULT_GATEWAY_BIND`. Hosted deployments must set the real public URL.
pub const DEFAULT_PUBLIC_BASE_URL: &str = "http://127.0.0.1:18080";

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
    public_base_url: String,
    oauth_token_client: Option<Arc<dyn OAuthTokenClient>>,
    oauth_metadata_client: Option<Arc<dyn OAuthMetadataClient>>,
    github_api_client: Option<Arc<dyn GitHubApiClient>>,
}

impl GatewayAgentApiBuilder {
    pub fn with_task_queue(mut self, task_queue: impl Into<String>) -> Self {
        self.task_queue = task_queue.into();
        self
    }

    /// Externally reachable base URL of this gateway, used to build the OAuth
    /// redirect URI (`{base}/auth/callback`).
    pub fn with_public_base_url(mut self, public_base_url: impl Into<String>) -> Self {
        self.public_base_url = public_base_url.into();
        self
    }

    /// Override the OAuth token-endpoint client (tests).
    pub fn with_oauth_token_client(mut self, token_client: Arc<dyn OAuthTokenClient>) -> Self {
        self.oauth_token_client = Some(token_client);
        self
    }

    /// Override the OAuth discovery/registration metadata client (tests).
    pub fn with_oauth_metadata_client(
        mut self,
        metadata_client: Arc<dyn OAuthMetadataClient>,
    ) -> Self {
        self.oauth_metadata_client = Some(metadata_client);
        self
    }

    /// Override the GitHub REST client (tests).
    pub fn with_github_api_client(mut self, github_api_client: Arc<dyn GitHubApiClient>) -> Self {
        self.github_api_client = Some(github_api_client);
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
        let token_client = self.oauth_token_client.unwrap_or_else(|| {
            Arc::new(
                HttpOAuthTokenClient::new()
                    .expect("construct OAuth token endpoint HTTP client"),
            )
        });
        let oauth_flows = OAuthFlowService::new(
            self.store.clone() as Arc<dyn OAuthClientStore>,
            self.store.clone() as Arc<dyn AuthFlowStore>,
            self.store.clone() as Arc<dyn AuthGrantStore>,
            self.store.clone() as Arc<dyn SecretStore>,
            token_client,
        );
        let metadata_client = self.oauth_metadata_client.unwrap_or_else(|| {
            Arc::new(
                HttpOAuthMetadataClient::new()
                    .expect("construct OAuth metadata HTTP client"),
            )
        });
        let mcp_oauth = McpOAuthDriver::new(
            self.store.clone() as Arc<dyn OAuthClientStore>,
            self.store.clone() as Arc<dyn SecretStore>,
            metadata_client,
        );
        let github_api = self.github_api_client.unwrap_or_else(|| {
            Arc::new(HttpGitHubApiClient::new().expect("construct GitHub REST HTTP client"))
        });
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
            public_base_url: self.public_base_url,
            oauth_flows,
            mcp_oauth,
            github_api,
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
    public_base_url: String,
    oauth_flows: OAuthFlowService,
    mcp_oauth: McpOAuthDriver,
    github_api: Arc<dyn GitHubApiClient>,
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
            public_base_url: DEFAULT_PUBLIC_BASE_URL.to_owned(),
            oauth_token_client: None,
            oauth_metadata_client: None,
            github_api_client: None,
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
        let task_queue = crate::config::task_queue_from_env()?;
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
        if effective_web_fetch_enabled(session_config) {
            config.web_fetch = WebFetchToolConfig::enabled();
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

fn effective_web_fetch_enabled(session_config: &SessionConfig) -> bool {
    session_config.tools.web_fetch.unwrap_or(true)
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

    async fn update_session_tools(
        &self,
        params: SessionToolsUpdateParams,
    ) -> Result<AgentApiOutcome<SessionToolsUpdateResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let loaded = self.load_session_state(&session_id).await?;
        self.require_open_idle_session(&session_id, &loaded, "tool update")?;
        if let Some(expected) = params.expected_tools_revision {
            let actual = loaded.state.tooling.revision;
            if expected != actual {
                return Err(AgentApiError::conflict(format!(
                    "expected tools revision {expected}, got {actual}"
                )));
            }
        }

        let update = tools_api::core_tool_update_from_api(params.update)?;
        update.validate_for(&loaded.state.tooling.tools)?;
        if update.is_empty() {
            return Ok(AgentApiOutcome::new(SessionToolsUpdateResponse {
                session: self.project_session_by_id(&session_id).await?,
            }));
        }

        let target_revision = loaded
            .state
            .tooling
            .revision
            .checked_add(1)
            .ok_or_else(|| AgentApiError::internal("tools revision exhausted"))?;
        let baseline_failures = self
            .query_status_optional(&session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(
            &session_id,
            update.into_command(params.expected_tools_revision),
        )
        .await?;
        let session = self
            .wait_for_tool_revision(&session_id, target_revision, baseline_failures)
            .await?;
        Ok(AgentApiOutcome::new(SessionToolsUpdateResponse { session }))
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

    async fn create_mcp_server(
        &self,
        params: McpServerCreateParams,
    ) -> Result<AgentApiOutcome<McpServerCreateResponse>, AgentApiError> {
        let record = create_mcp_server_record(params, now_ms()?)?;
        let server = self
            .store
            .create_server(record)
            .await
            .map_err(map_mcp_registry_error)?;
        Ok(AgentApiOutcome::new(McpServerCreateResponse {
            server: mcp_server_view(server),
        }))
    }

    async fn list_mcp_servers(
        &self,
        params: McpServerListParams,
    ) -> Result<AgentApiOutcome<McpServerListResponse>, AgentApiError> {
        let servers = self
            .store
            .list_servers(mcp_registry::ListMcpServers {
                status: params.status.map(mcp_api::registry_status_for_filter),
            })
            .await
            .map_err(map_mcp_registry_error)?
            .into_iter()
            .map(mcp_server_view)
            .collect();
        Ok(AgentApiOutcome::new(McpServerListResponse { servers }))
    }

    async fn read_mcp_server(
        &self,
        params: McpServerReadParams,
    ) -> Result<AgentApiOutcome<McpServerReadResponse>, AgentApiError> {
        let server_id = parse_mcp_server_id(params.server_id)?;
        let server = self
            .store
            .read_server(&server_id)
            .await
            .map_err(map_mcp_registry_error)?;
        Ok(AgentApiOutcome::new(McpServerReadResponse {
            server: mcp_server_view(server),
        }))
    }

    async fn delete_mcp_server(
        &self,
        params: McpServerDeleteParams,
    ) -> Result<AgentApiOutcome<McpServerDeleteResponse>, AgentApiError> {
        let server_id = parse_mcp_server_id(params.server_id)?;
        let server = self
            .store
            .delete_server(&server_id)
            .await
            .map_err(map_mcp_registry_error)?;
        Ok(AgentApiOutcome::new(McpServerDeleteResponse {
            server: mcp_server_view(server),
        }))
    }

    async fn link_session_mcp(
        &self,
        params: SessionMcpLinkParams,
    ) -> Result<AgentApiOutcome<SessionMcpLinkResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id.clone()).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let server_id = parse_mcp_server_id(params.server_id.clone())?;
        let server = self
            .store
            .read_server(&server_id)
            .await
            .map_err(map_mcp_registry_error)?;
        let grant = match params.auth_grant_id.clone() {
            Some(grant_id) => {
                let grant_id = parse_auth_grant_id(grant_id)?;
                Some(
                    self.store
                        .read_grant(&grant_id)
                        .await
                        .map_err(map_auth_registry_error)?,
                )
            }
            None => None,
        };
        let loaded = self.load_session_state(&session_id).await?;
        self.require_open_idle_session(&session_id, &loaded, "MCP link")?;

        let draft = session_mcp_link_from_record(params, &server, grant.as_ref())?;
        let link_tool_name = draft.tool_name.clone();
        let patch = apply_session_mcp_link(&loaded.state.tooling.tools, draft)?;
        let expected_tools = patch
            .apply_to(&loaded.state.tooling.tools)
            .map_err(|error| {
                AgentApiError::invalid_request(format!("invalid MCP tool patch: {error}"))
            })?;
        let expected_tool_ids = mcp_api::linked_session_mcp_tool_ids(&expected_tools);
        let baseline_failures = self
            .query_status_optional(&session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(
            &session_id,
            CoreAgentCommand::PatchTools {
                expected_revision: Some(loaded.state.tooling.revision),
                patch,
            },
        )
        .await?;
        let (session, links) = self
            .wait_for_session_mcp_links(&session_id, expected_tool_ids, baseline_failures)
            .await?;
        let link = links
            .iter()
            .find(|link| link.tool_id == link_tool_name.as_str())
            .cloned()
            .ok_or_else(|| {
                AgentApiError::internal(format!("linked MCP tool not visible: {link_tool_name}"))
            })?;
        Ok(AgentApiOutcome::new(SessionMcpLinkResponse {
            link,
            links,
            session,
        }))
    }

    async fn unlink_session_mcp(
        &self,
        params: SessionMcpUnlinkParams,
    ) -> Result<AgentApiOutcome<SessionMcpUnlinkResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let tool_name = parse_mcp_tool_name(params.tool_id)?;
        let loaded = self.load_session_state(&session_id).await?;
        self.require_open_idle_session(&session_id, &loaded, "MCP unlink")?;

        let patch = remove_session_mcp_link(&loaded.state.tooling.tools, &tool_name)?;
        let expected_tools = patch
            .apply_to(&loaded.state.tooling.tools)
            .map_err(|error| {
                AgentApiError::invalid_request(format!("invalid MCP tool patch: {error}"))
            })?;
        let expected_tool_ids = mcp_api::linked_session_mcp_tool_ids(&expected_tools);
        let baseline_failures = self
            .query_status_optional(&session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(
            &session_id,
            CoreAgentCommand::PatchTools {
                expected_revision: Some(loaded.state.tooling.revision),
                patch,
            },
        )
        .await?;
        let (session, links) = self
            .wait_for_session_mcp_links(&session_id, expected_tool_ids, baseline_failures)
            .await?;
        Ok(AgentApiOutcome::new(SessionMcpUnlinkResponse {
            tool_id: tool_name.as_str().to_owned(),
            links,
            session,
        }))
    }

    async fn list_session_mcp(
        &self,
        params: SessionMcpListParams,
    ) -> Result<AgentApiOutcome<SessionMcpListResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let loaded = self.load_session_state(&session_id).await?;
        Ok(AgentApiOutcome::new(SessionMcpListResponse {
            links: linked_session_mcp(&loaded.state.tooling.tools),
        }))
    }

    async fn import_auth_grant(
        &self,
        params: AuthGrantImportParams,
    ) -> Result<AgentApiOutcome<AuthGrantImportResponse>, AgentApiError> {
        let draft = auth_grant_import_draft(params, now_ms()?)?;
        self.store
            .put_secret(draft.secret.clone())
            .await
            .map_err(map_auth_registry_error)?;
        match self.store.create_grant(draft.grant).await {
            Ok(record) => Ok(AgentApiOutcome::new(AuthGrantImportResponse {
                grant: auth_grant_view(record),
            })),
            Err(error) => {
                // The secret is orphaned without its grant; clean up best-effort
                // so a failed import does not leave sealed values behind.
                let _ = self.store.delete_secret(&draft.secret.secret_id).await;
                Err(map_auth_registry_error(error))
            }
        }
    }

    async fn list_auth_grants(
        &self,
        params: AuthGrantListParams,
    ) -> Result<AgentApiOutcome<AuthGrantListResponse>, AgentApiError> {
        let grants = self
            .store
            .list_grants(auth_registry::ListAuthGrants {
                status: params.status.map(registry_auth_grant_status_for_filter),
            })
            .await
            .map_err(map_auth_registry_error)?;
        Ok(AgentApiOutcome::new(AuthGrantListResponse {
            grants: grants.into_iter().map(auth_grant_view).collect(),
        }))
    }

    async fn read_auth_grant(
        &self,
        params: AuthGrantReadParams,
    ) -> Result<AgentApiOutcome<AuthGrantReadResponse>, AgentApiError> {
        let grant_id = parse_auth_grant_id(params.grant_id)?;
        let record = self
            .store
            .read_grant(&grant_id)
            .await
            .map_err(map_auth_registry_error)?;
        Ok(AgentApiOutcome::new(AuthGrantReadResponse {
            grant: auth_grant_view(record),
        }))
    }

    async fn revoke_auth_grant(
        &self,
        params: AuthGrantRevokeParams,
    ) -> Result<AgentApiOutcome<AuthGrantRevokeResponse>, AgentApiError> {
        let grant_id = parse_auth_grant_id(params.grant_id)?;
        let record = self
            .store
            .update_grant_status(&grant_id, auth_registry::AuthGrantStatus::Revoked, now_ms()?)
            .await
            .map_err(map_auth_registry_error)?;
        Ok(AgentApiOutcome::new(AuthGrantRevokeResponse {
            grant: auth_grant_view(record),
        }))
    }

    async fn create_auth_client(
        &self,
        params: AuthClientCreateParams,
    ) -> Result<AgentApiOutcome<AuthClientCreateResponse>, AgentApiError> {
        let draft = auth_client_create_draft(params, now_ms()?)?;
        if let Some(secret) = &draft.secret {
            self.store
                .put_secret(secret.clone())
                .await
                .map_err(map_auth_registry_error)?;
        }
        match self.store.create_oauth_client(draft.client).await {
            Ok(record) => Ok(AgentApiOutcome::new(AuthClientCreateResponse {
                client: oauth_client_view(record),
            })),
            Err(error) => {
                // The secret is orphaned without its client; clean up
                // best-effort and surface the original failure.
                if let Some(secret) = &draft.secret {
                    let _ = self.store.delete_secret(&secret.secret_id).await;
                }
                Err(map_auth_registry_error(error))
            }
        }
    }

    async fn list_auth_clients(
        &self,
        _params: AuthClientListParams,
    ) -> Result<AgentApiOutcome<AuthClientListResponse>, AgentApiError> {
        let clients = self
            .store
            .list_oauth_clients()
            .await
            .map_err(map_auth_registry_error)?;
        Ok(AgentApiOutcome::new(AuthClientListResponse {
            clients: clients.into_iter().map(oauth_client_view).collect(),
        }))
    }

    async fn read_auth_client(
        &self,
        params: AuthClientReadParams,
    ) -> Result<AgentApiOutcome<AuthClientReadResponse>, AgentApiError> {
        let client_id = parse_oauth_client_id(params.client_id)?;
        let record = self
            .store
            .read_oauth_client(&client_id)
            .await
            .map_err(map_auth_registry_error)?;
        Ok(AgentApiOutcome::new(AuthClientReadResponse {
            client: oauth_client_view(record),
        }))
    }

    async fn delete_auth_client(
        &self,
        params: AuthClientDeleteParams,
    ) -> Result<AgentApiOutcome<AuthClientDeleteResponse>, AgentApiError> {
        let client_id = parse_oauth_client_id(params.client_id)?;
        let record = self
            .store
            .delete_oauth_client(&client_id)
            .await
            .map_err(map_auth_registry_error)?;
        // The stored client secret is unreachable without its client.
        if let Some(secret_id) = &record.client_secret {
            let _ = self.store.delete_secret(secret_id).await;
        }
        Ok(AgentApiOutcome::new(AuthClientDeleteResponse {
            client: oauth_client_view(record),
        }))
    }

    async fn start_auth_flow(
        &self,
        params: AuthFlowStartParams,
    ) -> Result<AgentApiOutcome<AuthFlowStartResponse>, AgentApiError> {
        // `mcp:<server_id>` lazily discovers and registers the OAuth client
        // for a catalogued MCP server before starting the flow.
        let client_id = match params.client_id.strip_prefix("mcp:") {
            Some(server_id) => self.ensure_mcp_oauth_client(server_id).await?,
            None => parse_oauth_client_id(params.client_id)?,
        };
        let started = self
            .oauth_flows
            .start_flow(StartAuthFlow {
                client_id,
                redirect_uri: oauth_redirect_uri(&self.public_base_url),
                scopes: params.scopes,
                audience: params.audience,
                principal: auth_registry::PrincipalRef::universe_default(),
            })
            .await
            .map_err(map_auth_registry_error)?;
        Ok(AgentApiOutcome::new(AuthFlowStartResponse {
            flow_id: started.flow.flow_id.as_str().to_owned(),
            authorize_url: started.authorize_url,
            expires_at_ms: started.flow.expires_at_ms,
        }))
    }

    async fn read_auth_flow_status(
        &self,
        params: AuthFlowStatusParams,
    ) -> Result<AgentApiOutcome<AuthFlowStatusResponse>, AgentApiError> {
        let flow_id = parse_auth_flow_id(params.flow_id)?;
        let record = self
            .oauth_flows
            .read_flow(&flow_id)
            .await
            .map_err(map_auth_registry_error)?;
        Ok(AgentApiOutcome::new(AuthFlowStatusResponse {
            flow: auth_flow_view(record, self.oauth_flows.now_ms()),
        }))
    }

    async fn create_auth_provider(
        &self,
        params: AuthProviderCreateParams,
    ) -> Result<AgentApiOutcome<AuthProviderCreateResponse>, AgentApiError> {
        let draft = auth_provider_create_draft(params, now_ms()?)?;
        // A model_oauth binding must point at a real, active grant; validate
        // before committing the provider row.
        if let auth_registry::AuthProviderConfig::ModelOAuth(config) = &draft.provider.config {
            let grant = self
                .store
                .read_grant(&config.grant_id)
                .await
                .map_err(map_auth_registry_error)?;
            if grant.status != auth_registry::AuthGrantStatus::Active {
                return Err(AgentApiError::rejected(format!(
                    "auth grant {} is not active: {:?}",
                    grant.grant_id, grant.status
                )));
            }
        }
        // The secret must exist before the provider row: auth_providers
        // carries a foreign key into auth_secrets.
        if let Some(secret) = &draft.secret {
            self.store
                .put_secret(secret.clone())
                .await
                .map_err(map_auth_registry_error)?;
        }
        match self.store.create_auth_provider(draft.provider).await {
            Ok(record) => Ok(AgentApiOutcome::new(AuthProviderCreateResponse {
                provider: auth_provider_view(record),
            })),
            Err(error) => {
                if let Some(secret) = &draft.secret {
                    let _ = self.store.delete_secret(&secret.secret_id).await;
                }
                Err(map_auth_registry_error(error))
            }
        }
    }

    async fn list_auth_providers(
        &self,
        _params: AuthProviderListParams,
    ) -> Result<AgentApiOutcome<AuthProviderListResponse>, AgentApiError> {
        let providers = self
            .store
            .list_auth_providers()
            .await
            .map_err(map_auth_registry_error)?;
        Ok(AgentApiOutcome::new(AuthProviderListResponse {
            providers: providers.into_iter().map(auth_provider_view).collect(),
        }))
    }

    async fn read_auth_provider(
        &self,
        params: AuthProviderReadParams,
    ) -> Result<AgentApiOutcome<AuthProviderReadResponse>, AgentApiError> {
        let provider_id = parse_auth_provider_id(params.provider_id)?;
        let record = self
            .store
            .read_auth_provider(&provider_id)
            .await
            .map_err(map_auth_registry_error)?;
        Ok(AgentApiOutcome::new(AuthProviderReadResponse {
            provider: auth_provider_view(record),
        }))
    }

    async fn delete_auth_provider(
        &self,
        params: AuthProviderDeleteParams,
    ) -> Result<AgentApiOutcome<AuthProviderDeleteResponse>, AgentApiError> {
        let provider_id = parse_auth_provider_id(params.provider_id)?;
        // The provider row must go first: its foreign key prevents deleting
        // the credential secret while the provider references it.
        let record = self
            .store
            .delete_auth_provider(&provider_id)
            .await
            .map_err(map_auth_registry_error)?;
        if let Some(secret_id) = &record.credential_secret {
            let _ = self.store.delete_secret(secret_id).await;
        }
        Ok(AgentApiOutcome::new(AuthProviderDeleteResponse {
            provider: auth_provider_view(record),
        }))
    }

    async fn list_github_installations(
        &self,
        params: AuthGitHubInstallationListParams,
    ) -> Result<AgentApiOutcome<AuthGitHubInstallationListResponse>, AgentApiError> {
        let (provider, app_jwt) = self.github_provider_jwt(params.provider_id).await?;
        let auth_registry::AuthProviderConfig::GitHubApp(config) = &provider.config else {
            return Err(AgentApiError::rejected(format!(
                "auth provider {} is not a github_app provider",
                provider.provider_id
            )));
        };
        let installations = self
            .github_api
            .list_installations(&config.api_base_url, &app_jwt)
            .await
            .map_err(map_github_app_error)?;
        Ok(AgentApiOutcome::new(AuthGitHubInstallationListResponse {
            installations: installations
                .iter()
                .map(github_installation_view)
                .collect(),
        }))
    }

    async fn grant_github_installation(
        &self,
        params: AuthGitHubInstallationGrantParams,
    ) -> Result<AgentApiOutcome<AuthGitHubInstallationGrantResponse>, AgentApiError> {
        let (provider, app_jwt) = self.github_provider_jwt(params.provider_id).await?;
        let auth_registry::AuthProviderConfig::GitHubApp(config) = &provider.config else {
            return Err(AgentApiError::rejected(format!(
                "auth provider {} is not a github_app provider",
                provider.provider_id
            )));
        };
        // Verify the installation exists live before recording the grant;
        // this also captures its account/permission metadata.
        let installations = self
            .github_api
            .list_installations(&config.api_base_url, &app_jwt)
            .await
            .map_err(map_github_app_error)?;
        let Some(installation) = installations
            .iter()
            .find(|installation| installation.installation_id == params.installation_id)
        else {
            return Err(AgentApiError::not_found(format!(
                "github app installation {} not found for provider {}",
                params.installation_id, provider.provider_id
            )));
        };
        let draft = github_installation_grant_draft(
            &provider,
            installation,
            params.grant_id,
            params.display_name,
            now_ms()?,
        )?;
        let record = self
            .store
            .create_grant(draft)
            .await
            .map_err(map_auth_registry_error)?;
        Ok(AgentApiOutcome::new(AuthGitHubInstallationGrantResponse {
            grant: auth_grant_view(record),
        }))
    }
}

/// Result of an authorization callback, consumed by the HTTP handler to
/// render a user-facing page. Never carries token material.
#[derive(Debug)]
pub enum OAuthCallbackOutcome {
    /// The flow completed and minted a grant.
    Completed { grant_id: String },
    /// The flow terminated without a grant (denial or failed exchange).
    Failed { message: String },
    /// The callback could not be matched to a live flow (unknown state,
    /// replay, or expiry).
    Rejected { message: String },
}

impl GatewayAgentApi {
    /// Lazily discover and register the OAuth client for an OAuth-protected
    /// MCP server (P69 G4): protected resource metadata, authorization
    /// server metadata, then CIMD or dynamic client registration. Existing
    /// `mcp:<server_id>` client records are reused without network traffic.
    async fn ensure_mcp_oauth_client(
        &self,
        server_id: &str,
    ) -> Result<auth_registry::OAuthClientId, AgentApiError> {
        // A manually registered `mcp:<server_id>` client always wins: reuse
        // it without touching the catalog or the network, so login works
        // even when the catalog record is named differently or absent.
        let client_id =
            auth_registry::mcp_oauth_client_id(server_id).map_err(map_auth_registry_error)?;
        match self.store.read_oauth_client(&client_id).await {
            Ok(existing) => return Ok(existing.client_id),
            Err(auth_registry::AuthRegistryError::ClientNotFound { .. }) => {}
            Err(error) => return Err(map_auth_registry_error(error)),
        }

        let server_id = parse_mcp_server_id(server_id.to_owned())?;
        let record = self
            .store
            .read_server(&server_id)
            .await
            .map_err(map_mcp_registry_error)?;
        let target = mcp_oauth_target_from_record(&record)?;
        let redirect_uri = oauth_redirect_uri(&self.public_base_url);
        let cimd = cimd_config(&self.public_base_url);
        let client = self
            .mcp_oauth
            .ensure_client(&target, &redirect_uri, cimd.as_ref())
            .await
            .map_err(map_mcp_oauth_error)?;
        Ok(client.client_id)
    }

    /// The Client ID Metadata Document served at
    /// `/auth/client-metadata.json` for authorization servers that support
    /// CIMD client ids.
    pub fn cimd_document(&self) -> serde_json::Value {
        oauth_api::cimd_document(&self.public_base_url)
    }

    /// Load a GitHub App provider and sign its app JWT for control-plane
    /// calls (installation listing/verification). The JWT and the key only
    /// exist in memory inside [`auth_registry::SecretValue`] wrappers.
    async fn github_provider_jwt(
        &self,
        provider_id: String,
    ) -> Result<(auth_registry::AuthProviderRecord, auth_registry::SecretValue), AgentApiError>
    {
        let provider_id = parse_auth_provider_id(provider_id)?;
        let provider = self
            .store
            .read_auth_provider(&provider_id)
            .await
            .map_err(map_auth_registry_error)?;
        let auth_registry::AuthProviderConfig::GitHubApp(config) = &provider.config else {
            return Err(AgentApiError::rejected(format!(
                "auth provider {provider_id} is not a github_app provider"
            )));
        };
        let Some(credential_secret) = &provider.credential_secret else {
            return Err(AgentApiError::rejected(format!(
                "auth provider {provider_id} has no private key credential"
            )));
        };
        let (_, private_key) = self
            .store
            .read_secret(credential_secret)
            .await
            .map_err(map_auth_registry_error)?;
        let app_jwt = auth_registry::sign_github_app_jwt(&config.app_id, &private_key, now_ms()?)
            .map_err(map_github_app_error)?;
        Ok((provider, app_jwt))
    }

    /// Handle the OAuth redirect: consume the flow, exchange the code, and
    /// store the resulting grant. Called by the gateway's HTTP callback
    /// route, not via JSON-RPC.
    pub async fn complete_oauth_callback(
        &self,
        callback: auth_registry::AuthCallback,
    ) -> OAuthCallbackOutcome {
        match self.oauth_flows.complete_callback(callback).await {
            Ok(record) => match (&record.grant_id, &record.error) {
                (Some(grant_id), _) => OAuthCallbackOutcome::Completed {
                    grant_id: grant_id.as_str().to_owned(),
                },
                (None, Some(error)) => OAuthCallbackOutcome::Failed {
                    message: error.clone(),
                },
                (None, None) => OAuthCallbackOutcome::Failed {
                    message: "authorization flow ended without an outcome".to_owned(),
                },
            },
            Err(error) => OAuthCallbackOutcome::Rejected {
                message: map_auth_registry_error(error).message,
            },
        }
    }
}
#[cfg(test)]
mod tests;
