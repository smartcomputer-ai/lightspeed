//! `api` gateway for the Temporal-backed agent workflow.

mod api_config;
mod auth_api;
mod blobs;
mod common;
mod environment_credentials;
mod environment_lifecycle;
mod environment_projection;
pub(crate) mod environment_providers;
mod environments;
mod errors;
mod github_api;
mod host_controllers;
mod input;
mod instructions;
mod mcp_api;
mod models_api;
mod oauth_api;
mod parse;
mod profiles;
mod prompts;
mod session_jobs;
mod session_toolset;
mod skills;
mod vfs_api;
mod workflow;

use api_config::engine_session_config_from_api;
#[cfg(test)]
use api_config::*;
use auth_api::{
    api_auth_provider_kind, auth_grant_import_draft, auth_grant_view, map_auth_error,
    parse_auth_grant_id, registry_auth_grant_status_for_filter,
};
use blobs::{has_blobs, put_blobs, read_blob};
use common::now_ms;
use environment_lifecycle::parse_registry_environment_id;
use environment_providers::{map_environments_error, parse_environment_provider_id};
use environments::{activate_environment_command, deactivate_environment_command};
use errors::*;
use github_api::{
    auth_provider_create_draft, auth_provider_view, github_installation_grant_draft,
    github_installation_view, map_github_app_error, parse_auth_provider_id,
};
use host_controllers::{HostControllerConnector, WebSocketHostControllerConnector};
use input::{context_entry_input_from_api, run_input_from_api};
use mcp_api::{map_mcp_error, mcp_server_view, parse_mcp_server_id, put_mcp_server_record};
use models_api::{ModelDiscoveryService, stored_provider_key_resolver};
use oauth_api::{
    auth_client_create_draft, auth_flow_view, cimd_config, map_mcp_oauth_error,
    mcp_oauth_target_from_record, oauth_client_view, oauth_redirect_uri, parse_auth_flow_id,
    parse_oauth_client_id,
};
use parse::*;
use session_toolset::store_tool_documents;
use skills::{
    active_skill_catalog_ref, active_skill_ids, active_skill_ids_after_remove,
    active_skill_ids_after_upsert, skill_activation_context_input,
};
#[cfg(test)]
use skills::{skill_active_response, skill_list_response};
use vfs_api::{commit_vfs_snapshot, read_vfs_snapshot, vfs_workspace_view};

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    sync::Arc,
    time::{Duration, Instant},
};

use api::*;
use api::{
    SkillActivationScope as ApiSkillActivationScope,
    SkillActivationSource as ApiSkillActivationSource,
};
use api_projection::{
    CoreAgentProjector, MAX_EVENT_PAGE_LIMIT, ProjectSession, api_kind_from_str, api_run_id,
    decode_stored_entry, event_cursor, event_page_limit, map_session_store_error, parse_api_run_id,
    project_context_entry_inputs, read_all_session_entries, replay_core_agent_state,
};
use async_trait::async_trait;
use auth::{
    AuthFlowStore, AuthGrantStore, AuthProviderStore, GitHubApiClient, HttpGitHubApiClient,
    HttpOAuthMetadataClient, HttpOAuthTokenClient, McpOAuthDriver, OAuthClientStore,
    OAuthFlowService, OAuthMetadataClient, OAuthTokenClient, SecretStore, StartAuthFlow,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use engine::{
    BlobRef, BoundWorkflowToolDispatch, CompactionPolicy, ContextEntry, ContextEntryInput,
    ContextEntryKey, ContextEntryKind, ContextMessageRole, CoreAgentCommand, CoreAgentStatus,
    FunctionToolSpec, ManagedSessionWorkflowTools, ModelSelection, ProviderApiKind, RunConfig,
    RunId, RunStatus, SKILL_ACTIVATION_PROVIDER_KIND_RUN, SKILL_ACTIVATION_PROVIDER_KIND_SESSION,
    SKILL_CATALOG_CONTEXT_KEY, SessionConfig, SessionId, SkillId, SubmissionId, ToolChoice,
    ToolKind, ToolName, ToolParallelism, ToolSpec, WorkflowEndpointRef, WorkflowStartRef,
    WorkflowToolCompletion, WorkflowToolCompletionKeySource, WorkflowToolDeclaration,
    WorkflowToolDefinition, WorkflowToolId, WorkflowToolTarget, skill_activation_context_key,
    storage::{BlobStore, BlobStoreError, ReadSessionEvents, SessionStore},
};
use llm_clients::{anthropic::messages as anthropic, openai::responses as openai};
use mcp::McpRegistryStore;
use store_pg::PgStore;
use temporalio_client::{
    Client, WorkflowDescribeOptions, WorkflowHandle, WorkflowQueryOptions, WorkflowSignalOptions,
    WorkflowStartOptions, WorkflowTerminateOptions, errors::WorkflowInteractionError,
    errors::WorkflowQueryError, errors::WorkflowStartError,
};
use temporalio_common::protos::temporal::api::enums::v1::WorkflowExecutionStatus;
use tools::{
    builtin::{BuiltinTool, BuiltinToolOperation},
    environment::jobs::{
        JOB_RUN_DEADLINE_AFTER_MS, JOB_RUN_WORKFLOW_SEMANTIC_TYPE, JOB_RUN_WORKFLOW_TOOL_ID,
        JOB_SUBMIT_WORKFLOW_SEMANTIC_TYPE, JOB_SUBMIT_WORKFLOW_TOOL_ID,
    },
    runtime::{ToolDocument, ToolTarget},
    skills::{
        SkillCatalogSnapshot, SkillMetadata, configured_vfs_skill_root_specs,
        resolve_linked_vfs_skill_roots, skill_catalog_context_input,
    },
    toolset::{
        ResolvedToolset, ToolsetConfig, ToolsetEnvironment, enable_concurrency_for_workflow_tools,
        materialize_workflow_tools, resolve_toolset,
    },
    web::fetch::WebFetchToolConfig,
    web::search::OpenAiResponsesWebSearchConfig,
    workflow_tool::{
        validate_workflow_tool_definition_documents, validate_workflow_tool_reply_schema,
    },
};
use vfs::{
    CompareAndSetVfsWorkspaceHead, CreateVfsWorkspaceRecord, VfsCatalogError, VfsSnapshotRecord,
    VfsSnapshotSource, VfsSnapshotStore, VfsWorkspaceId, VfsWorkspaceRecord, VfsWorkspaceStore,
};

use super::{
    AgentAdmission, AgentAdmissionFailure, AgentAdmissionFailureKind, AgentSessionArgs,
    AgentSessionStatus, AgentSessionWorkflow, DEFAULT_TASK_QUEUE, DEFAULT_TEMPORAL_NAMESPACE,
    DEFAULT_TEMPORAL_TARGET, connect_temporal, default_model_from_env, pg_store_from_env,
};

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);
const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(90);
/// Server-side cap for `session/events/read` long-poll waits. Requests above
/// the cap are clamped, not rejected. The gateway HTTP request timeout must
/// stay above this cap.
const DEFAULT_EVENTS_WAIT_CAP: Duration = Duration::from_secs(30);
/// Cap on `activationText` returned per `session/context/append` entry. The committed
/// context blob is authoritative; activation text only needs enough of the
/// head for trigger matching.
const ACTIVATION_TEXT_MAX_BYTES: usize = 4096;
/// `session/list` page size when the request does not specify one.
const DEFAULT_SESSION_LIST_LIMIT: usize = 50;
/// Server-side cap for `session/list` page sizes; larger requests are clamped.
const MAX_SESSION_LIST_LIMIT: usize = 200;
/// Resource bound for the opaque managed-controller run terminal token.
const MAX_RUN_TERMINAL_NOTIFICATION_TOKEN_BYTES: usize = 512;

/// Default public base URL for the gateway-hosted OAuth callback; matches
/// `DEFAULT_GATEWAY_BIND`. Hosted deployments must set the real public URL.
pub const DEFAULT_PUBLIC_BASE_URL: &str = "http://127.0.0.1:18080";

fn session_summary_view(record: engine::storage::SessionRecord) -> SessionSummaryView {
    SessionSummaryView {
        id: record.session_id.as_str().to_owned(),
        display_name: record.display_name,
        lifecycle_status: match record.lifecycle_status {
            engine::storage::SessionLifecycleStatus::New => SessionLifecycleStatus::New,
            engine::storage::SessionLifecycleStatus::Open => SessionLifecycleStatus::Open,
            engine::storage::SessionLifecycleStatus::Closed => SessionLifecycleStatus::Closed,
        },
        managed: record.managed,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

/// Opaque `session/list` cursor: `{updated_at_ms}:{session_id}`. Session ids
/// cannot contain `:` at the first position of the numeric prefix, so
/// `split_once` is unambiguous.
fn encode_session_list_cursor(cursor: &engine::storage::SessionListCursor) -> String {
    format!("{}:{}", cursor.updated_at_ms, cursor.session_id)
}

fn decode_session_list_cursor(
    cursor: &str,
) -> Result<engine::storage::SessionListCursor, AgentApiError> {
    let invalid =
        || AgentApiError::invalid_request(format!("invalid session list cursor: {cursor}"));
    let (updated_at_ms, session_id) = cursor.split_once(':').ok_or_else(invalid)?;
    let updated_at_ms = updated_at_ms.parse::<u64>().map_err(|_| invalid())?;
    let session_id = SessionId::try_new(session_id).map_err(|_| invalid())?;
    Ok(engine::storage::SessionListCursor {
        updated_at_ms,
        session_id,
    })
}

fn status_has_submission(
    status: Option<&AgentSessionStatus>,
    submission_id: &SubmissionId,
) -> bool {
    let Some(status) = status else {
        return false;
    };
    status
        .active_run
        .as_ref()
        .is_some_and(|run| run.submission_id.as_ref() == Some(submission_id))
        || status
            .queued_runs
            .iter()
            .any(|run| run.submission_id.as_ref() == Some(submission_id))
        || status
            .completed_runs
            .iter()
            .any(|run| run.submission_id.as_ref() == Some(submission_id))
        || status
            .consumed_message_submissions
            .iter()
            .any(|consumed| &consumed.submission_id == submission_id)
}

enum ExistingRunSubmission {
    ReturnRun { run_id: RunId, status: RunStatus },
    Reject,
}

pub(super) enum ContextAppendWaitOutcome {
    Applied { entry: ContextEntryInput },
    Failed { failure: AgentAdmissionFailure },
}

fn existing_run_submission(
    state: &engine::CoreAgentState,
    submission_id: &SubmissionId,
    source: &engine::RunRequestSource,
    run_config: &RunConfig,
    notify_on_terminal: &[engine::RunTerminalNotifyIntent],
) -> Option<ExistingRunSubmission> {
    if let Some(active) = state
        .runs
        .active
        .as_ref()
        .filter(|run| run.submission_id.as_ref() == Some(submission_id))
    {
        return Some(
            if active.origin == engine::RunOrigin::Requested
                && active.source.matches_request(source)
                && &active.run_config == run_config
                && active.notify_on_terminal == notify_on_terminal
            {
                ExistingRunSubmission::ReturnRun {
                    run_id: active.run_id,
                    status: active.status,
                }
            } else {
                ExistingRunSubmission::Reject
            },
        );
    }
    if let Some(queued) = state
        .runs
        .queued
        .iter()
        .find(|run| run.submission_id.as_ref() == Some(submission_id))
    {
        if queued.origin != engine::RunOrigin::Requested
            || !queued.source.matches_request(source)
            || &queued.run_config != run_config
            || queued.notify_on_terminal != notify_on_terminal
        {
            return Some(ExistingRunSubmission::Reject);
        }
        return None;
    }
    if let Some(completed) = state
        .runs
        .completed
        .iter()
        .find(|run| run.submission_id.as_ref() == Some(submission_id))
    {
        let digest = engine::request_run_submission_digest(source, run_config, notify_on_terminal);
        return Some(match completed.submission_digest {
            Some(existing) if existing != digest => ExistingRunSubmission::Reject,
            _ => ExistingRunSubmission::ReturnRun {
                run_id: completed.run_id,
                status: completed.status,
            },
        });
    }
    if let Some(message) = state
        .runs
        .messages
        .iter()
        .find(|message| message.submission_id.as_ref() == Some(submission_id))
    {
        let digest = engine::request_run_submission_digest(source, run_config, notify_on_terminal);
        return Some(match message.submission_digest {
            existing if existing != digest => ExistingRunSubmission::Reject,
            _ => ExistingRunSubmission::Reject,
        });
    }
    None
}

fn duplicate_submission_error(submission_id: &SubmissionId) -> AgentApiError {
    AgentApiError::rejected(format!(
        "submission id {submission_id} was already used with a different command, input, or run config"
    ))
}

async fn context_append_result(
    store: &dyn BlobStore,
    key: String,
    status: ContextAppendStatus,
    input: &ContextEntryInput,
    submitted_text: Option<&str>,
) -> Result<ContextAppendResult, AgentApiError> {
    let entry = project_context_entry_inputs(std::slice::from_ref(input))
        .into_iter()
        .next();
    let activation_text = if is_audio_transcript_entry(input) {
        let text = store
            .read_text(&input.content_ref)
            .await
            .map_err(map_input_blob_store_error)?;
        Some(crate::transcript::transcript_activation_text(&text).to_owned())
    } else if context_append_entry_has_activation_text(input) {
        // The submitted text is reused when it produced this exact entry so
        // plain-text appends do not pay a blob read per response entry.
        match submitted_text {
            Some(text) => Some(text.to_owned()),
            None => Some(
                store
                    .read_text(&input.content_ref)
                    .await
                    .map_err(map_input_blob_store_error)?,
            ),
        }
    } else {
        None
    };
    let (activation_text, activation_text_truncated) = match activation_text {
        Some(text) => {
            let (text, truncated) = capped_activation_text(text);
            (Some(text), truncated)
        }
        None => (None, false),
    };
    Ok(ContextAppendResult {
        key,
        status,
        entry,
        failure: None,
        activation_text,
        activation_text_truncated,
    })
}

fn capped_activation_text(text: String) -> (String, bool) {
    if text.len() <= ACTIVATION_TEXT_MAX_BYTES {
        return (text, false);
    }
    let mut end = ACTIVATION_TEXT_MAX_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

fn context_append_entry_has_activation_text(input: &ContextEntryInput) -> bool {
    matches!(
        &input.kind,
        ContextEntryKind::Message {
            role: ContextMessageRole::User
        }
    ) && input.preview.is_none()
        && input
            .media_type
            .as_deref()
            .map(|media_type| {
                let media_type = media_type.trim().to_ascii_lowercase();
                media_type.is_empty() || media_type == "text/plain"
            })
            .unwrap_or(true)
}

fn context_append_failed_result(
    key: String,
    failure: InputAdmissionFailureView,
) -> ContextAppendResult {
    ContextAppendResult {
        key,
        status: ContextAppendStatus::Failed,
        entry: None,
        failure: Some(failure),
        activation_text: None,
        activation_text_truncated: false,
    }
}

fn active_entry_input(entry: &ContextEntry) -> ContextEntryInput {
    ContextEntryInput {
        kind: entry.kind.clone(),
        content_ref: entry.content_ref.clone(),
        media_type: entry.media_type.clone(),
        preview: entry.preview.clone(),
        provider_kind: entry.provider_kind.clone(),
        provider_item_id: entry.provider_item_id.clone(),
        token_estimate: entry.token_estimate.clone(),
    }
}

fn active_context_entry_matches_input(active: &ContextEntry, input: &ContextEntryInput) -> bool {
    let active_input = active_entry_input(active);
    active_input == *input || audio_input_matches_transcript(input, &active_input)
}

fn audio_input_matches_transcript(input: &ContextEntryInput, active: &ContextEntryInput) -> bool {
    input
        .media_type
        .as_deref()
        .is_some_and(|mime| mime.trim().to_ascii_lowercase().starts_with("audio/"))
        && is_audio_transcript_entry(active)
        && active.provider_item_id.as_deref() == Some(input.content_ref.as_str())
}

fn is_audio_transcript_entry(input: &ContextEntryInput) -> bool {
    input.provider_kind.as_deref() == Some(crate::transcript::AUDIO_TRANSCRIPT_PROVIDER_KIND)
}

fn input_admission_failure_from_api_error(error: AgentApiError) -> InputAdmissionFailureView {
    let kind = match error.kind {
        AgentApiErrorKind::UnsupportedAudioMime => InputAdmissionFailureKind::UnsupportedAudioMime,
        AgentApiErrorKind::AudioBlobTooLarge => InputAdmissionFailureKind::BlobTooLarge,
        AgentApiErrorKind::AudioDurationTooLong => InputAdmissionFailureKind::AudioDurationTooLong,
        AgentApiErrorKind::TranscoderUnavailable => {
            InputAdmissionFailureKind::TranscoderUnavailable
        }
        AgentApiErrorKind::TranscodeFailure => InputAdmissionFailureKind::TranscodeFailure,
        AgentApiErrorKind::TranscriptionFailure => InputAdmissionFailureKind::TranscriptionFailure,
        AgentApiErrorKind::NotFound => InputAdmissionFailureKind::BlobMissing,
        _ => InputAdmissionFailureKind::UnsupportedMedia,
    };
    InputAdmissionFailureView {
        kind,
        message: error.message,
    }
}

fn input_admission_failure_from_workflow(
    failure: &AgentAdmissionFailure,
) -> InputAdmissionFailureView {
    let kind = match failure.kind {
        AgentAdmissionFailureKind::UnsupportedAudioMime => {
            InputAdmissionFailureKind::UnsupportedAudioMime
        }
        AgentAdmissionFailureKind::AudioBlobMissing => InputAdmissionFailureKind::BlobMissing,
        AgentAdmissionFailureKind::AudioBlobTooLarge => InputAdmissionFailureKind::BlobTooLarge,
        AgentAdmissionFailureKind::AudioDurationTooLong => {
            InputAdmissionFailureKind::AudioDurationTooLong
        }
        AgentAdmissionFailureKind::TranscoderUnavailable => {
            InputAdmissionFailureKind::TranscoderUnavailable
        }
        AgentAdmissionFailureKind::TranscodeFailure => InputAdmissionFailureKind::TranscodeFailure,
        AgentAdmissionFailureKind::TranscriptionFailure => {
            InputAdmissionFailureKind::TranscriptionFailure
        }
        AgentAdmissionFailureKind::RejectedCommand => InputAdmissionFailureKind::AdmissionRejected,
    };
    InputAdmissionFailureView {
        kind,
        message: failure.message.clone(),
    }
}

pub struct GatewayAgentApiBuilder {
    client: Client,
    store: Arc<PgStore>,
    task_queue: String,
    default_model: ModelSelection,
    continue_as_new_history_threshold: Option<u32>,
    poll_interval: Duration,
    operation_timeout: Duration,
    events_wait_cap: Duration,
    public_base_url: String,
    oauth_token_client: Option<Arc<dyn OAuthTokenClient>>,
    oauth_metadata_client: Option<Arc<dyn OAuthMetadataClient>>,
    github_api_client: Option<Arc<dyn GitHubApiClient>>,
    model_discovery_openai: Option<Arc<openai::Client>>,
    model_discovery_anthropic: Option<Arc<anthropic::Client>>,
    host_controller_connector: Arc<dyn HostControllerConnector>,
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

    /// Use deployment-shared LLM clients for direct provider model discovery.
    pub fn with_model_discovery_clients(
        mut self,
        openai: Arc<openai::Client>,
        anthropic: Arc<anthropic::Client>,
    ) -> Self {
        self.model_discovery_openai = Some(openai);
        self.model_discovery_anthropic = Some(anthropic);
        self
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn with_host_controller_connector(
        mut self,
        connector: Arc<dyn HostControllerConnector>,
    ) -> Self {
        self.host_controller_connector = connector;
        self
    }

    pub fn with_default_model(mut self, model: ModelSelection) -> Self {
        self.default_model = model;
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

    pub fn with_events_wait_cap(mut self, events_wait_cap: Duration) -> Self {
        self.events_wait_cap = events_wait_cap;
        self
    }

    pub fn build(self) -> GatewayAgentApi {
        let token_client = self.oauth_token_client.unwrap_or_else(|| {
            Arc::new(
                HttpOAuthTokenClient::new().expect("construct OAuth token endpoint HTTP client"),
            )
        });
        let oauth_flows = OAuthFlowService::new(
            self.store.clone() as Arc<dyn OAuthClientStore>,
            self.store.clone() as Arc<dyn AuthFlowStore>,
            self.store.clone() as Arc<dyn AuthGrantStore>,
            self.store.clone() as Arc<dyn SecretStore>,
            token_client.clone(),
        );
        let metadata_client = self.oauth_metadata_client.unwrap_or_else(|| {
            Arc::new(HttpOAuthMetadataClient::new().expect("construct OAuth metadata HTTP client"))
        });
        let mcp_oauth = McpOAuthDriver::new(
            self.store.clone() as Arc<dyn OAuthClientStore>,
            self.store.clone() as Arc<dyn SecretStore>,
            metadata_client,
        );
        let github_api = self.github_api_client.unwrap_or_else(|| {
            Arc::new(HttpGitHubApiClient::new().expect("construct GitHub REST HTTP client"))
        });
        let discovery_openai = self.model_discovery_openai.unwrap_or_else(|| {
            Arc::new(
                openai::Client::new(openai::Config::from_env_allow_missing_key())
                    .expect("construct OpenAI model discovery client"),
            )
        });
        let discovery_anthropic = self.model_discovery_anthropic.unwrap_or_else(|| {
            Arc::new(
                anthropic::Client::new(anthropic::Config::from_env_allow_missing_key())
                    .expect("construct Anthropic model discovery client"),
            )
        });
        let model_discovery = ModelDiscoveryService::new(
            discovery_openai,
            discovery_anthropic,
            stored_provider_key_resolver(
                self.store.clone(),
                token_client.clone(),
                github_api.clone(),
            ),
        );
        GatewayAgentApi {
            client: self.client,
            store: self.store,
            task_queue: self.task_queue,
            default_model: self.default_model,
            continue_as_new_history_threshold: self.continue_as_new_history_threshold,
            poll_interval: self.poll_interval,
            operation_timeout: self.operation_timeout,
            events_wait_cap: self.events_wait_cap,
            public_base_url: self.public_base_url,
            oauth_flows,
            mcp_oauth,
            github_api,
            model_discovery,
            host_controller_connector: self.host_controller_connector,
        }
    }
}

pub struct GatewayAgentApi {
    client: Client,
    store: Arc<PgStore>,
    task_queue: String,
    default_model: ModelSelection,
    continue_as_new_history_threshold: Option<u32>,
    poll_interval: Duration,
    operation_timeout: Duration,
    events_wait_cap: Duration,
    public_base_url: String,
    oauth_flows: OAuthFlowService,
    mcp_oauth: McpOAuthDriver,
    github_api: Arc<dyn GitHubApiClient>,
    model_discovery: ModelDiscoveryService,
    host_controller_connector: Arc<dyn HostControllerConnector>,
}

impl GatewayAgentApi {
    pub fn builder(client: Client, store: Arc<PgStore>) -> GatewayAgentApiBuilder {
        GatewayAgentApiBuilder {
            client,
            store,
            task_queue: DEFAULT_TASK_QUEUE.to_owned(),
            default_model: default_model_from_env(),
            continue_as_new_history_threshold: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
            events_wait_cap: DEFAULT_EVENTS_WAIT_CAP,
            public_base_url: DEFAULT_PUBLIC_BASE_URL.to_owned(),
            oauth_token_client: None,
            oauth_metadata_client: None,
            github_api_client: None,
            model_discovery_openai: None,
            model_discovery_anthropic: None,
            host_controller_connector: Arc::new(WebSocketHostControllerConnector::default()),
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
        // `start_session` is idempotent on client-supplied session ids; this
        // wrapper remains for callers predating that behavior.
        self.start_session(params).await
    }

    fn allocate_session_id(&self) -> SessionId {
        SessionId::new(format!("session_{}", uuid::Uuid::new_v4().simple()))
    }

    fn allocate_submission_id(&self) -> SubmissionId {
        SubmissionId::new(format!("submit_{}", uuid::Uuid::new_v4().simple()))
    }

    /// Materialize the session's granted features into the provider-aware
    /// toolset. Absent feature = no tools: capability semantics need no
    /// effective-default resolution here.
    fn session_toolset_config(
        &self,
        session_config: &SessionConfig,
        include_environment_tools: bool,
        include_job_read_tool: bool,
    ) -> ToolsetConfig {
        let features = &session_config.features;
        let mut config = ToolsetConfig::empty();
        config.environment_read = features.environments.is_some();
        config.environment_selection = features
            .environments
            .as_ref()
            .is_some_and(|environments| environments.selection_tools);
        config.builtin = match features.vfs.as_ref().and_then(|vfs| vfs.tools) {
            None => tools::toolset::BuiltinToolsetConfig::disabled(),
            Some(engine::VfsToolSurface::ReadOnly) => tools::toolset::BuiltinToolsetConfig {
                vfs: tools::toolset::FilesystemToolsetConfig::read_only(),
                ..tools::toolset::BuiltinToolsetConfig::disabled()
            },
            Some(engine::VfsToolSurface::Edit) => tools::toolset::BuiltinToolsetConfig::workspace(),
        };
        if let Some(web) = features.web.as_ref() {
            if web.search.is_some()
                && session_config.model.api_kind == engine::ProviderApiKind::OpenAiResponses
            {
                config.openai_web_search = OpenAiResponsesWebSearchConfig::cached();
            }
            if web.fetch.is_some() {
                config.web_fetch = WebFetchToolConfig::enabled();
            }
        }
        if features.fleet.is_some() {
            config.fleet = tools::fleet::FleetToolsetConfig::enabled();
        }
        if features.timers.is_some() || features.fleet.is_some() {
            // Fleet waiting depends on the base concurrency tools, so the
            // fleet grant implies them; the timers grant adds nothing extra
            // today beyond the same surface.
            config.concurrency = tools::concurrency::ConcurrencyToolsetConfig::timer();
        }
        if include_environment_tools {
            config.builtin.environment = tools::toolset::EnvironmentToolsetConfig::basic();
        }
        if include_job_read_tool {
            config.builtin.environment.job_read = true;
        }
        config
    }

    fn workflow_args(
        &self,
        session_id: SessionId,
        display_name: Option<String>,
        session_config: SessionConfig,
        workflow_tools: Option<ManagedSessionWorkflowTools>,
        close_on_terminal: bool,
    ) -> AgentSessionArgs {
        AgentSessionArgs {
            universe_id: self.universe_id(),
            session_id,
            display_name,
            session_config,
            workflow_tools,
            legacy_max_steps_per_input: None,
            continue_as_new_history_threshold: self.continue_as_new_history_threshold,
            close_on_terminal,
            continuation_state: None,
        }
    }

    pub(crate) async fn start_session_for_fleet_with_profile(
        &self,
        session_id: &SessionId,
        close_on_terminal: bool,
        profile: Option<ProfileSource>,
    ) -> Result<(), AgentApiError> {
        let workflow_tools = match self.load_session_state(session_id).await {
            Ok(loaded) => managed_workflow_tools_from_state(&loaded.state)?,
            Err(error) if is_not_found(&error) => None,
            Err(error) => return Err(error),
        };
        self.start_session_internal(
            SessionStartParams {
                session_id: Some(session_id.as_str().to_owned()),
                display_name: None,
                config: None,
                profile,
            },
            close_on_terminal,
            workflow_tools,
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
                session_id: Some(session_id.as_str().to_owned()),
                display_name: None,
                config: None,
                profile,
            },
            close_on_terminal,
            Some(workflow_tools),
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn enqueue_run_for_fleet(
        &self,
        session_id: &SessionId,
        input: Vec<InputItem>,
        submission_id: SubmissionId,
        notify_on_terminal: Vec<engine::RunTerminalNotifyIntent>,
    ) -> Result<String, AgentApiError> {
        let input = run_input_from_api(self.store.as_ref(), &input).await?;
        let source = engine::RunRequestSource::Input { input };
        let status_before_signal = self.query_status_optional(session_id).await?;
        let baseline_admission_failures = status_before_signal
            .as_ref()
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(
            session_id,
            CoreAgentCommand::RequestRun(engine::RunRequestCommand {
                notify_on_terminal,
                submission_id: Some(submission_id.clone()),
                source,
                run_config: RunConfig::default(),
            }),
        )
        .await?;
        let run_id = self
            .wait_for_run_admitted(session_id, &submission_id, baseline_admission_failures)
            .await?;
        Ok(format!("run_{}", run_id.as_u64()))
    }

    pub(crate) async fn deliver_message_for_fleet(
        &self,
        session_id: &SessionId,
        input: Vec<InputItem>,
        submission_id: SubmissionId,
    ) -> Result<(), AgentApiError> {
        let input = run_input_from_api(self.store.as_ref(), &input).await?;
        let status_before_signal = self.query_status_optional(session_id).await?;
        let baseline_admission_failures = status_before_signal
            .as_ref()
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(
            session_id,
            CoreAgentCommand::SubmitMessage(engine::SubmitMessageCommand {
                submission_id: Some(submission_id.clone()),
                input,
            }),
        )
        .await?;
        self.wait_for_message_admitted(session_id, &submission_id, baseline_admission_failures)
            .await
    }

    /// Fleet-internal run start: identical to the public `session/runs/start`
    /// boundary except that the admitted `RunRequestCommand` carries the
    /// spawn's cross-session notify-intents. Public callers can request only
    /// the single destination derived from the durable lifecycle controller.
    pub(crate) async fn start_run_for_fleet(
        &self,
        session_id: &SessionId,
        input: Vec<InputItem>,
        submission_id: SubmissionId,
        notify_on_terminal: Vec<engine::RunTerminalNotifyIntent>,
    ) -> Result<String, AgentApiError> {
        let response = self
            .start_run_internal(
                RunStartParams {
                    session_id: session_id.as_str().to_owned(),
                    source: RunStartSource::Input { items: input },
                    submission_id: Some(submission_id.as_str().to_owned()),
                    config: None,
                    notify_on_terminal: None,
                },
                notify_on_terminal,
            )
            .await?;
        Ok(response.result.run.id)
    }

    async fn start_run_internal(
        &self,
        params: RunStartParams,
        internal_notify_on_terminal: Vec<engine::RunTerminalNotifyIntent>,
    ) -> Result<AgentApiOutcome<RunStartResponse>, AgentApiError> {
        let RunStartParams {
            session_id,
            source,
            submission_id,
            config,
            notify_on_terminal,
        } = params;
        let session_id = SessionId::try_new(session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let loaded = self
            .load_session_state_with_current_run_context(&session_id)
            .await?;
        let notify_on_terminal = run_terminal_notify_intents(
            loaded.state.workflow_tools.lifecycle_controller.as_ref(),
            notify_on_terminal,
            internal_notify_on_terminal,
        )?;
        let client_supplied_submission_id = submission_id.is_some();
        let submission_id = match submission_id {
            Some(submission_id) => SubmissionId::try_new(submission_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid submission id: {error}"))
            })?,
            None => self.allocate_submission_id(),
        };
        let session_config = loaded.state.lifecycle.config.as_ref().ok_or_else(|| {
            AgentApiError::invalid_request(format!("session is not open: {session_id}"))
        })?;
        let run_config = api_config::run_config_for_start(session_config, config)?;
        let source = match source {
            RunStartSource::Input { items } => engine::RunRequestSource::Input {
                input: run_input_from_api(self.store.as_ref(), &items).await?,
            },
            RunStartSource::Context { keys } => {
                if keys.is_empty() {
                    return Err(AgentApiError::invalid_request(
                        "session/runs/start source=context requires at least one key",
                    ));
                }
                let mut parsed = Vec::with_capacity(keys.len());
                let mut seen = BTreeSet::new();
                for key in keys {
                    let key = ContextEntryKey::try_new(key).map_err(|error| {
                        AgentApiError::invalid_request(format!("invalid context key: {error}"))
                    })?;
                    if !seen.insert(key.clone()) {
                        return Err(AgentApiError::invalid_request(format!(
                            "duplicate trigger context key: {key}"
                        )));
                    }
                    parsed.push(key);
                }
                engine::RunRequestSource::Context { keys: parsed }
            }
        };
        if let Some(existing) = existing_run_submission(
            &loaded.state,
            &submission_id,
            &source,
            &run_config,
            &notify_on_terminal,
        ) {
            return match existing {
                ExistingRunSubmission::ReturnRun { run_id, status } => {
                    let run = self.project_run_by_id(&session_id, run_id, status).await?;
                    Ok(AgentApiOutcome::new(RunStartResponse { run }))
                }
                ExistingRunSubmission::Reject => Err(duplicate_submission_error(&submission_id)),
            };
        }
        if loaded.state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        let status_before_signal = self.query_status_optional(&session_id).await?;
        let baseline_admission_failures = status_before_signal
            .as_ref()
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        let wait_for_admission_drain = client_supplied_submission_id
            || status_has_submission(status_before_signal.as_ref(), &submission_id);
        self.submit_core_command(
            &session_id,
            CoreAgentCommand::RequestRun(engine::RunRequestCommand {
                notify_on_terminal,
                submission_id: Some(submission_id.clone()),
                source,
                run_config,
            }),
        )
        .await?;
        let run = self
            .wait_for_run_accepted(
                &session_id,
                &submission_id,
                baseline_admission_failures,
                wait_for_admission_drain,
            )
            .await?;
        Ok(AgentApiOutcome::new(RunStartResponse { run }))
    }

    async fn start_session_internal(
        &self,
        params: SessionStartParams,
        close_on_terminal: bool,
        trusted_workflow_tools: Option<ManagedSessionWorkflowTools>,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        let SessionStartParams {
            session_id,
            display_name,
            config,
            profile,
        } = params;
        let workflow_tools = trusted_workflow_tools;
        let client_supplied_id = session_id.is_some();
        let session_id = match session_id {
            Some(session_id) => SessionId::try_new(session_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid session id: {error}"))
            })?,
            None => self.allocate_session_id(),
        };
        if let Some(workflow_tools) = workflow_tools.as_ref() {
            self.validate_managed_session_declaration(workflow_tools)?;
        }
        if client_supplied_id {
            match self.load_session_state(&session_id).await {
                Ok(loaded) if loaded.state.lifecycle.status == CoreAgentStatus::Closed => {
                    if let Some(workflow_tools) = workflow_tools.as_ref() {
                        validate_managed_session_retry(
                            &loaded.state,
                            self.universe_id(),
                            workflow_tools,
                        )?;
                    }
                    let session = self.project_session_by_id(&session_id).await?;
                    return Ok(AgentApiOutcome::new(SessionStartResponse { session }));
                }
                Ok(loaded) => {
                    if let Some(workflow_tools) = workflow_tools.as_ref() {
                        validate_managed_session_retry(
                            &loaded.state,
                            self.universe_id(),
                            workflow_tools,
                        )?;
                    }
                }
                Err(error) if is_not_found(&error) => {}
                Err(error) => return Err(error),
            }
        }
        let resolved_profile = match profile {
            Some(source) => Some(self.resolve_profile_source(source).await?),
            None => None,
        };
        let start_config = self.merge_profile_start_config(
            resolved_profile
                .as_ref()
                .and_then(|profile| profile.document.config.clone()),
            config,
        );
        let session_config = self.session_config_for_start(start_config).await?;
        if let Some(workflow_tools) = workflow_tools.as_ref() {
            self.validate_managed_session_materialization(&session_config, workflow_tools)
                .await?;
        }
        let started = self
            .client
            .start_workflow(
                AgentSessionWorkflow::run,
                self.workflow_args(
                    session_id.clone(),
                    display_name,
                    session_config,
                    workflow_tools.clone(),
                    close_on_terminal,
                ),
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
                let loaded = self.load_session_state(&session_id).await?;
                if let Some(workflow_tools) = workflow_tools.as_ref() {
                    validate_managed_session_retry(
                        &loaded.state,
                        self.universe_id(),
                        workflow_tools,
                    )?;
                }
                if loaded.state.lifecycle.status == CoreAgentStatus::Closed {
                    let session = self.project_session_by_id(&session_id).await?;
                    return Ok(AgentApiOutcome::new(SessionStartResponse { session }));
                }
                let session = self.wait_for_open_session(&session_id).await?;
                return Ok(AgentApiOutcome::new(SessionStartResponse { session }));
            }
            Err(error) => return Err(error),
        }
        self.wait_for_open_session(&session_id).await?;
        let loaded = self.load_session_state(&session_id).await?;
        if let Some(workflow_tools) = workflow_tools.as_ref() {
            validate_managed_session_retry(&loaded.state, self.universe_id(), workflow_tools)?;
        }
        let _ = self.configure_session_toolset(&session_id, &loaded).await?;
        if let Some(profile) = resolved_profile {
            self.apply_profile_document(&session_id, &profile.document, false, None, None)
                .await?;
        }
        self.load_session_state_with_current_run_context(&session_id)
            .await?;
        let session = self.project_session_by_id(&session_id).await?;
        Ok(AgentApiOutcome::new(SessionStartResponse { session }))
    }

    async fn core_environment_job_workflow_tool_declarations(
        &self,
    ) -> Result<Vec<WorkflowToolDeclaration>, AgentApiError> {
        let target = ToolTarget::api_kind(ProviderApiKind::OpenAiResponses);
        let recipe_bytes = serde_json::to_vec(&temporal_workflow::WorkflowToolRecipeV1 {
            workflow_type: "EnvironmentJobWorkflow".to_owned(),
            task_queue: self.task_queue.clone(),
        })
        .map_err(|error| {
            AgentApiError::internal(format!(
                "encode core environment-job workflow recipe: {error}"
            ))
        })?;
        let recipe_fingerprint = temporal_workflow::workflow_tool_recipe_fingerprint(&recipe_bytes);
        let recipe_ref = self
            .store
            .put_bytes(recipe_bytes)
            .await
            .map_err(map_blob_store_error)?;

        let definitions = [
            (
                BuiltinToolOperation::JobSubmit,
                JOB_SUBMIT_WORKFLOW_TOOL_ID,
                JOB_SUBMIT_WORKFLOW_SEMANTIC_TYPE,
                WorkflowToolCompletion::Promises {
                    reply_schema_ref: None,
                    deadline_after_ms: None,
                    max_promises: engine::MAX_COMPLETION_PROMISES,
                    key_source: WorkflowToolCompletionKeySource::ArrayIndices {
                        pointer: "/jobs".to_owned(),
                        prefix: "job-".to_owned(),
                    },
                },
            ),
            (
                BuiltinToolOperation::JobRun,
                JOB_RUN_WORKFLOW_TOOL_ID,
                JOB_RUN_WORKFLOW_SEMANTIC_TYPE,
                WorkflowToolCompletion::Joined {
                    reply_schema_ref: None,
                    deadline_after_ms: JOB_RUN_DEADLINE_AFTER_MS,
                },
            ),
        ];
        let mut declarations = Vec::with_capacity(definitions.len());
        for (operation, tool_id, semantic_type, completion) in definitions {
            let bundle = BuiltinTool::environment_canonical(operation)
                .spec_bundle(&target, false)
                .map_err(|error| {
                    AgentApiError::internal(format!("build core {tool_id} tool: {error}"))
                })?;
            store_tool_documents(self.store.as_ref(), &bundle.documents).await?;
            let tool = bundle.spec;
            declarations.push(WorkflowToolDeclaration::new(
                WorkflowToolDefinition {
                    tool_id: WorkflowToolId::new(tool_id),
                    revision: 1,
                    semantic_type: semantic_type.to_owned(),
                    tool,
                },
                WorkflowToolTarget::Start {
                    start: WorkflowStartRef {
                        recipe_format: temporal_workflow::WORKFLOW_TOOL_RECIPE_FORMAT_V1,
                        revision: 1,
                        recipe_ref: recipe_ref.clone(),
                        recipe_fingerprint: recipe_fingerprint.clone(),
                    },
                },
                completion,
            ));
        }
        Ok(declarations)
    }

    async fn ensure_core_environment_job_workflow_tools(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
    ) -> Result<(), AgentApiError> {
        if has_all_core_environment_job_bindings(state) {
            return Ok(());
        }
        let baseline_failures = self
            .query_status_optional(session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        let declarations = self
            .core_environment_job_workflow_tool_declarations()
            .await?;
        for declaration in declarations {
            if state
                .workflow_tools
                .bindings
                .contains_key(&declaration.definition.tool_id)
            {
                continue;
            }
            self.submit_core_command(
                session_id,
                CoreAgentCommand::AdmitSystemWorkflowTool {
                    session_universe_id: self.universe_id(),
                    declaration,
                },
            )
            .await?;
        }
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for core environment-job workflow tool admission: {session_id}"
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
            if has_all_core_environment_job_bindings(&loaded.state) {
                return Ok(());
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    fn validate_managed_session_declaration(
        &self,
        workflow_tools: &ManagedSessionWorkflowTools,
    ) -> Result<(), AgentApiError> {
        workflow_tools.admit(self.universe_id()).map_err(|error| {
            AgentApiError::invalid_request(format!(
                "invalid managed-session workflow-tool declaration: {error}"
            ))
        })?;
        Ok(())
    }

    async fn validate_managed_session_materialization(
        &self,
        session_config: &SessionConfig,
        workflow_tools: &ManagedSessionWorkflowTools,
    ) -> Result<(), AgentApiError> {
        let admitted = workflow_tools.admit(self.universe_id()).map_err(|error| {
            AgentApiError::invalid_request(format!(
                "invalid managed-session workflow-tool declaration: {error}"
            ))
        })?;
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

        let target = ToolTarget::from(&session_config.model);
        let mut config = self.session_toolset_config(session_config, false, false);
        enable_concurrency_for_workflow_tools(&mut config, materialized_bindings.iter().copied());
        let mut toolset = resolve_toolset(ToolsetEnvironment { target: &target }, &config)
            .map_err(|error| {
                AgentApiError::invalid_request(format!("build session tools: {error}"))
            })?;
        materialize_workflow_tools(&mut toolset, materialized_bindings.iter().copied()).map_err(
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
        self.projector()
            .project_session(ProjectSession {
                session_id,
                state: &loaded.state,
                record: &loaded.record,
                entries: &loaded.entries,
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
            .or_else(|| {
                loaded
                    .state
                    .runs
                    .active
                    .as_ref()
                    .filter(|run| run.run_id == run_id)
                    .map(|run| run.status)
            })
            .unwrap_or(fallback_status);
        self.projector()
            .project_run(&loaded.entries, run_id, status)
            .await
    }
}

pub(super) struct LoadedSession {
    pub(super) record: engine::storage::SessionRecord,
    pub(super) entries: Vec<engine::CoreAgentEntry>,
    pub(super) state: engine::CoreAgentState,
}

fn managed_workflow_tools_from_api(
    input: ManagedSessionWorkflowToolsInput,
) -> Result<ManagedSessionWorkflowTools, AgentApiError> {
    if input.lifecycle_controller.is_none() && input.tools.is_empty() {
        return Err(AgentApiError::invalid_request(
            "managed session requires a lifecycle controller or at least one workflow tool",
        ));
    }
    let lifecycle_controller = input.lifecycle_controller.map(workflow_endpoint_from_api);
    let tools = input
        .tools
        .into_iter()
        .map(|declaration| {
            let tool_id =
                WorkflowToolId::try_new(declaration.definition.tool_id).map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid workflow tool id: {error}"))
                })?;
            if is_core_environment_job_tool_id(tool_id.as_str()) {
                return Err(AgentApiError::invalid_request(format!(
                    "workflow tool id {tool_id} is reserved"
                )));
            }
            let tool_name =
                ToolName::try_new(declaration.definition.tool.name).map_err(|error| {
                    AgentApiError::invalid_request(format!(
                        "invalid workflow tool {tool_id} name: {error}"
                    ))
                })?;
            let parallelism = match declaration.definition.tool.parallelism {
                ToolParallelismView::Exclusive => ToolParallelism::Exclusive,
                ToolParallelismView::ParallelSafe => ToolParallelism::ParallelSafe,
            };
            let kind = match declaration.definition.tool.kind {
                WorkflowToolKindInput::Function {
                    description_ref,
                    input_schema_ref,
                    output_schema_ref,
                    strict,
                    provider_options_ref,
                } => ToolKind::Function(FunctionToolSpec {
                    description_ref: parse_workflow_tool_blob_ref(
                        &tool_id,
                        "descriptionRef",
                        description_ref,
                    )?,
                    input_schema_ref: parse_required_workflow_tool_blob_ref(
                        &tool_id,
                        "inputSchemaRef",
                        input_schema_ref,
                    )?,
                    output_schema_ref: parse_workflow_tool_blob_ref(
                        &tool_id,
                        "outputSchemaRef",
                        output_schema_ref,
                    )?,
                    strict,
                    provider_options_ref: parse_workflow_tool_blob_ref(
                        &tool_id,
                        "providerOptionsRef",
                        provider_options_ref,
                    )?,
                }),
            };
            let target = match declaration.target {
                WorkflowToolTargetInput::Bound { receiver, dispatch } => {
                    WorkflowToolTarget::Bound {
                        receiver: workflow_endpoint_from_api(receiver),
                        dispatch: match dispatch {
                            BoundWorkflowToolDispatchInput::Pull => BoundWorkflowToolDispatch::Pull,
                            BoundWorkflowToolDispatchInput::Push => BoundWorkflowToolDispatch::Push,
                        },
                    }
                }
                WorkflowToolTargetInput::Start { start } => WorkflowToolTarget::Start {
                    start: WorkflowStartRef {
                        recipe_format: start.recipe_format,
                        revision: start.revision,
                        recipe_ref: parse_required_workflow_tool_blob_ref(
                            &tool_id,
                            "target.start.recipeRef",
                            start.recipe_ref,
                        )?,
                        recipe_fingerprint: start.recipe_fingerprint,
                    },
                },
            };
            let completion = match declaration.completion {
                WorkflowToolCompletionInput::Accepted => WorkflowToolCompletion::Accepted,
                WorkflowToolCompletionInput::Joined {
                    reply_schema_ref,
                    deadline_after_ms,
                } => WorkflowToolCompletion::Joined {
                    reply_schema_ref: parse_workflow_tool_blob_ref(
                        &tool_id,
                        "completion.replySchemaRef",
                        reply_schema_ref,
                    )?,
                    deadline_after_ms,
                },
                WorkflowToolCompletionInput::Promises {
                    reply_schema_ref,
                    deadline_after_ms,
                    max_promises,
                    key_source,
                } => WorkflowToolCompletion::Promises {
                    reply_schema_ref: parse_workflow_tool_blob_ref(
                        &tool_id,
                        "completion.replySchemaRef",
                        reply_schema_ref,
                    )?,
                    deadline_after_ms,
                    max_promises,
                    key_source: match key_source {
                        WorkflowToolCompletionKeySourceInput::Reply => {
                            WorkflowToolCompletionKeySource::Reply
                        }
                        WorkflowToolCompletionKeySourceInput::StringArray { pointer } => {
                            WorkflowToolCompletionKeySource::StringArray { pointer }
                        }
                        WorkflowToolCompletionKeySourceInput::ArrayIndices { pointer, prefix } => {
                            WorkflowToolCompletionKeySource::ArrayIndices { pointer, prefix }
                        }
                    },
                },
            };
            Ok(WorkflowToolDeclaration::new(
                WorkflowToolDefinition {
                    tool_id,
                    revision: declaration.definition.revision,
                    semantic_type: declaration.definition.semantic_type,
                    tool: ToolSpec {
                        name: tool_name,
                        kind,
                        parallelism,
                        execution: engine::ToolExecutionSpec::default(),
                    },
                },
                target,
                completion,
            ))
        })
        .collect::<Result<Vec<_>, AgentApiError>>()?;
    Ok(ManagedSessionWorkflowTools {
        version: input.version,
        lifecycle_controller,
        tools,
    })
}

fn workflow_endpoint_from_api(input: WorkflowEndpointInput) -> WorkflowEndpointRef {
    WorkflowEndpointRef {
        workflow_id: input.workflow_id,
        workflow_kind: input.workflow_kind,
    }
}

fn parse_required_workflow_tool_blob_ref(
    tool_id: &WorkflowToolId,
    field: &str,
    value: String,
) -> Result<BlobRef, AgentApiError> {
    BlobRef::parse(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid workflow tool {tool_id} {field}: {error}"))
    })
}

fn parse_workflow_tool_blob_ref(
    tool_id: &WorkflowToolId,
    field: &str,
    value: Option<String>,
) -> Result<Option<BlobRef>, AgentApiError> {
    value
        .map(|value| parse_required_workflow_tool_blob_ref(tool_id, field, value))
        .transpose()
}

fn run_terminal_notify_intents(
    lifecycle_controller: Option<&WorkflowEndpointRef>,
    notification: Option<RunTerminalNotificationInput>,
    internal: Vec<engine::RunTerminalNotifyIntent>,
) -> Result<Vec<engine::RunTerminalNotifyIntent>, AgentApiError> {
    let Some(notification) = notification else {
        return Ok(internal);
    };
    if !internal.is_empty() {
        return Err(AgentApiError::invalid_request(
            "run terminal notification cannot be combined with internal notify intents",
        ));
    }
    let controller = lifecycle_controller.ok_or_else(|| {
        AgentApiError::invalid_request(
            "notifyOnTerminal requires a managed session lifecycle controller",
        )
    })?;
    if notification.token.is_empty() {
        return Err(AgentApiError::invalid_request(
            "notifyOnTerminal token must not be empty",
        ));
    }
    if notification.token.len() > MAX_RUN_TERMINAL_NOTIFICATION_TOKEN_BYTES {
        return Err(AgentApiError::invalid_request(format!(
            "notifyOnTerminal token is too long: {} bytes, max {}",
            notification.token.len(),
            MAX_RUN_TERMINAL_NOTIFICATION_TOKEN_BYTES
        )));
    }
    Ok(vec![engine::RunTerminalNotifyIntent {
        holder_workflow_id: controller.workflow_id.clone(),
        token: notification.token,
    }])
}

fn validate_managed_session_retry(
    state: &engine::CoreAgentState,
    session_universe_id: uuid::Uuid,
    workflow_tools: &ManagedSessionWorkflowTools,
) -> Result<(), AgentApiError> {
    let expected = workflow_tools
        .creation_fingerprint(session_universe_id)
        .map_err(|error| {
            AgentApiError::invalid_request(format!(
                "invalid managed-session workflow-tool declaration: {error}"
            ))
        })?;
    match (
        state.workflow_tools.session_universe_id,
        state.workflow_tools.managed_creation_fingerprint.as_deref(),
    ) {
        (Some(actual_universe), Some(actual))
            if actual_universe == session_universe_id && actual == expected =>
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

fn managed_workflow_tools_from_state(
    state: &engine::CoreAgentState,
) -> Result<Option<ManagedSessionWorkflowTools>, AgentApiError> {
    let Some(version) = state.workflow_tools.managed_declaration_version else {
        let has_non_system_bindings = state
            .workflow_tools
            .bindings
            .keys()
            .any(|tool_id| !state.workflow_tools.system_binding_ids.contains(tool_id));
        if state.workflow_tools.session_universe_id.is_none()
            && state.workflow_tools.managed_creation_fingerprint.is_none()
            && state.workflow_tools.lifecycle_controller.is_none()
            && !has_non_system_bindings
        {
            return Ok(None);
        }
        return Err(AgentApiError::internal(
            "session has incomplete managed workflow-tool creation state",
        ));
    };
    if state.workflow_tools.session_universe_id.is_none()
        || state.workflow_tools.managed_creation_fingerprint.is_none()
    {
        return Err(AgentApiError::internal(
            "session has incomplete managed workflow-tool creation state",
        ));
    }
    Ok(Some(ManagedSessionWorkflowTools {
        version,
        lifecycle_controller: state.workflow_tools.lifecycle_controller.clone(),
        tools: state
            .workflow_tools
            .bindings
            .iter()
            .filter(|(tool_id, _)| !state.workflow_tools.system_binding_ids.contains(*tool_id))
            .map(|(_, binding)| binding)
            .map(|binding| WorkflowToolDeclaration {
                definition: binding.definition.clone(),
                target: binding.target.clone(),
                completion: binding.completion.clone(),
            })
            .collect(),
    }))
}

fn is_core_environment_job_tool_id(tool_id: &str) -> bool {
    matches!(
        tool_id,
        JOB_SUBMIT_WORKFLOW_TOOL_ID | JOB_RUN_WORKFLOW_TOOL_ID
    )
}

fn is_core_environment_job_binding(binding: &engine::WorkflowToolBinding) -> bool {
    is_core_environment_job_tool_id(binding.definition.tool_id.as_str())
}

fn has_all_core_environment_job_bindings(state: &engine::CoreAgentState) -> bool {
    [JOB_SUBMIT_WORKFLOW_TOOL_ID, JOB_RUN_WORKFLOW_TOOL_ID]
        .into_iter()
        .all(|tool_id| {
            state
                .workflow_tools
                .bindings
                .contains_key(&WorkflowToolId::new(tool_id))
        })
}

#[async_trait]
impl AgentApiService for GatewayAgentApi {
    async fn list_models(
        &self,
        params: ModelListParams,
    ) -> Result<AgentApiOutcome<ModelListResponse>, AgentApiError> {
        Ok(AgentApiOutcome::new(
            self.model_discovery.list(params.selectable_only).await,
        ))
    }

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
                name: "lightspeed-agent".to_owned(),
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

    /// Idempotent on a client-supplied session id: when the session already
    /// exists, the existing session view is returned (any `config` in the
    /// retried request is ignored; session config is applied only at
    /// creation). This keeps a retried `session/start` + `session/runs/start` pair
    /// safe end to end.
    async fn start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        self.start_session_internal(params, false, None).await
    }

    async fn start_managed_session(
        &self,
        params: ManagedSessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        let ManagedSessionStartParams {
            session_id,
            display_name,
            config,
            profile,
            workflow_tools,
        } = params;
        let workflow_tools = managed_workflow_tools_from_api(workflow_tools)?;
        self.start_session_internal(
            SessionStartParams {
                session_id,
                display_name,
                config,
                profile,
            },
            false,
            Some(workflow_tools),
        )
        .await
    }

    async fn create_profile(
        &self,
        params: ProfileCreateParams,
    ) -> Result<AgentApiOutcome<ProfileCreateResponse>, AgentApiError> {
        self.create_profile_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn read_profile(
        &self,
        params: ProfileReadParams,
    ) -> Result<AgentApiOutcome<ProfileReadResponse>, AgentApiError> {
        self.read_profile_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn list_profiles(
        &self,
        params: ProfileListParams,
    ) -> Result<AgentApiOutcome<ProfileListResponse>, AgentApiError> {
        self.list_profile_records(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn put_profile(
        &self,
        params: ProfilePutParams,
    ) -> Result<AgentApiOutcome<ProfilePutResponse>, AgentApiError> {
        self.put_profile_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn delete_profile(
        &self,
        params: ProfileDeleteParams,
    ) -> Result<AgentApiOutcome<ProfileDeleteResponse>, AgentApiError> {
        self.delete_profile_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn apply_profile(
        &self,
        params: ProfileApplyParams,
    ) -> Result<AgentApiOutcome<ProfileApplyResponse>, AgentApiError> {
        self.apply_profile_to_session(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn put_session_config(
        &self,
        params: SessionConfigPutParams,
    ) -> Result<AgentApiOutcome<SessionConfigPutResponse>, AgentApiError> {
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
        let config = engine_session_config_from_api(params.config, self.default_model.clone())?;
        config
            .validate()
            .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
        // Declared MCP links must resolve (catalog record, grant/policy
        // compatibility) before the document enters the session log.
        self.desired_mcp_tools(&config.features).await?;
        self.validate_workspace_link_targets(&config.features)
            .await?;
        if &config == current_config {
            // The config event is an idempotent no-op, but derived managed
            // context may still need repair after an interrupted refresh.
            self.load_session_state_with_current_run_context(&session_id)
                .await?;
            return Ok(AgentApiOutcome::new(SessionConfigPutResponse {
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
        self.submit_core_command(
            &session_id,
            CoreAgentCommand::ReplaceSessionConfig {
                expected_revision: Some(loaded.state.lifecycle.config_revision),
                config,
            },
        )
        .await?;
        self.wait_for_config_revision(&session_id, target_revision, baseline_failures)
            .await?;
        let loaded = self.load_session_state(&session_id).await?;
        let _ = self.configure_session_toolset(&session_id, &loaded).await?;
        self.load_session_state_with_current_run_context(&session_id)
            .await?;
        let session = self.project_session_by_id(&session_id).await?;
        Ok(AgentApiOutcome::new(SessionConfigPutResponse { session }))
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

    async fn list_sessions(
        &self,
        params: SessionListParams,
    ) -> Result<AgentApiOutcome<SessionListResponse>, AgentApiError> {
        let limit = match params.limit {
            Some(0) => {
                return Err(AgentApiError::invalid_request("limit must be positive"));
            }
            Some(limit) => (limit as usize).min(MAX_SESSION_LIST_LIMIT),
            None => DEFAULT_SESSION_LIST_LIMIT,
        };
        let cursor = params
            .cursor
            .as_deref()
            .map(decode_session_list_cursor)
            .transpose()?;
        let page = self
            .store
            .list_sessions(engine::storage::ListSessions { cursor, limit })
            .await
            .map_err(map_session_store_error)?;
        Ok(AgentApiOutcome::new(SessionListResponse {
            sessions: page
                .sessions
                .into_iter()
                .map(session_summary_view)
                .collect(),
            next_cursor: page.next_cursor.as_ref().map(encode_session_list_cursor),
        }))
    }

    async fn rename_session(
        &self,
        params: SessionRenameParams,
    ) -> Result<AgentApiOutcome<SessionRenameResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let record = self
            .store
            .set_session_display_name(&session_id, params.display_name)
            .await
            .map_err(map_session_store_error)?;
        Ok(AgentApiOutcome::new(SessionRenameResponse {
            session: session_summary_view(record),
        }))
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
        // Long-poll: clamp the requested wait to the server cap and park
        // until an event lands past the cursor or the deadline passes. A
        // `session/close` appends a lifecycle event, so parked readers
        // observe closes as a normal wakeup.
        let wait = Duration::from_millis(params.wait_ms.unwrap_or(0)).min(self.events_wait_cap);
        let deadline = Instant::now() + wait;
        loop {
            let page = self
                .store
                .read_after(ReadSessionEvents {
                    session_id: session_id.clone(),
                    after: params.after.map(|cursor| engine::EventSeq::new(cursor.seq)),
                    limit,
                })
                .await
                .map_err(map_session_store_error)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if page.entries.is_empty() && !remaining.is_zero() {
                let poll = self
                    .poll_interval
                    .min(Duration::from_millis(250))
                    .min(remaining);
                tokio::time::sleep(poll).await;
                continue;
            }
            let head_cursor = self
                .store
                .head(&session_id)
                .await
                .map_err(map_session_store_error)?
                .map(|position| event_cursor(position.seq));
            let codec = engine::CoreAgentCodec;
            let mut events = Vec::with_capacity(page.entries.len());
            for entry in &page.entries {
                let entry = decode_stored_entry(&codec, entry)?;
                events.push(self.projector().project_entry(&session_id, &entry).await?);
            }

            return Ok(AgentApiOutcome::new(SessionEventsReadResponse {
                events,
                next_cursor: page.next_after.map(event_cursor),
                head_cursor,
                complete: page.complete,
                gap: None,
            }));
        }
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
        if !params.force {
            if loaded.state.runs.active.is_some() || !loaded.state.runs.queued.is_empty() {
                return Err(AgentApiError::rejected(
                    "session cannot close with active work",
                ));
            }
            self.submit_core_command(&session_id, CoreAgentCommand::CloseSession { force: false })
                .await?;
            let session = self.wait_for_closed_session(&session_id).await?;
            return Ok(AgentApiOutcome::new(SessionCloseResponse { session }));
        }

        // Force path. Prefer the live workflow: it cancels active work,
        // appends the close, observes closed+quiescent, and exits itself.
        if self.workflow_is_running(&session_id).await {
            let signalled = self
                .submit_core_command(&session_id, CoreAgentCommand::CloseSession { force: true })
                .await
                .is_ok();
            if signalled {
                if let Ok(session) = self.wait_for_closed_session(&session_id).await {
                    return Ok(AgentApiOutcome::new(SessionCloseResponse { session }));
                }
            }
            // The workflow exists but never converged: it is wedged (e.g. a
            // permanently failing workflow task). Terminate it so the direct
            // append below is the only writer, then reconcile the log.
            let _ = self
                .workflow_handle(&session_id)
                .terminate(WorkflowTerminateOptions::default())
                .await;
        }
        // No running workflow (operator terminate, bootstrap failure, or the
        // terminate above): reconcile the session log directly. Session and
        // run status are projections of the log, so this alone recovers the
        // row; the expected-head CAS protects against a concurrent writer.
        self.force_close_session_in_store(&session_id).await?;
        let session = self.project_session_by_id(&session_id).await?;
        Ok(AgentApiOutcome::new(SessionCloseResponse { session }))
    }

    async fn delete_session(
        &self,
        params: SessionDeleteParams,
    ) -> Result<AgentApiOutcome<SessionDeleteResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let record = self
            .store
            .delete_closed_session(&session_id)
            .await
            .map_err(map_session_store_error)?;
        Ok(AgentApiOutcome::new(SessionDeleteResponse {
            session: session_summary_view(record),
        }))
    }

    async fn compact_context(
        &self,
        params: ContextCompactParams,
    ) -> Result<AgentApiOutcome<ContextCompactResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let loaded = self.load_session_state(&session_id).await?;
        self.require_open_idle_session(&session_id, &loaded, "context compaction")?;
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

    async fn append_context(
        &self,
        params: ContextAppendParams,
    ) -> Result<AgentApiOutcome<ContextAppendResponse>, AgentApiError> {
        const MAX_CONTEXT_APPEND_ENTRIES: usize = 64;

        enum PreparedAppend {
            Ready {
                key: ContextEntryKey,
                input: ContextEntryInput,
                /// Submitted text kept in hand so the response does not
                /// re-read the blob it was just written from. Only valid for
                /// the entry it produced (checked via `content_ref`).
                text: Option<String>,
            },
            Failed(ContextAppendResult),
        }

        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        if params.entries.is_empty() {
            return Err(AgentApiError::invalid_request(
                "session/context/append requires at least one entry",
            ));
        }
        if params.entries.len() > MAX_CONTEXT_APPEND_ENTRIES {
            return Err(AgentApiError::invalid_request(format!(
                "session/context/append accepts at most {MAX_CONTEXT_APPEND_ENTRIES} entries per call"
            )));
        }

        let mut prepared = Vec::with_capacity(params.entries.len());
        let mut seen_keys = BTreeSet::new();
        for entry in &params.entries {
            let key = ContextEntryKey::try_new(entry.key.clone()).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid context key: {error}"))
            })?;
            if !seen_keys.insert(key.clone()) {
                return Err(AgentApiError::invalid_request(format!(
                    "duplicate context key in append batch: {key}"
                )));
            }
            match context_entry_input_from_api(self.store.as_ref(), &entry.item).await {
                Ok(input) => {
                    let text = match &entry.item {
                        InputItem::Text { text } => Some(text.trim().to_owned()),
                        _ => None,
                    };
                    prepared.push(PreparedAppend::Ready { key, input, text });
                }
                Err(error) if matches!(entry.item, InputItem::Media { .. }) => {
                    prepared.push(PreparedAppend::Failed(context_append_failed_result(
                        key.as_str().to_owned(),
                        input_admission_failure_from_api_error(error),
                    )));
                }
                Err(error) => return Err(error),
            }
        }

        let loaded = self.load_session_state(&session_id).await?;
        if loaded.state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        let mut ordered = Vec::with_capacity(prepared.len());
        let mut pending = Vec::new();
        for prepared in prepared {
            match prepared {
                PreparedAppend::Failed(result) => ordered.push(PreparedAppend::Failed(result)),
                PreparedAppend::Ready { key, input, text } => {
                    if let Some(active) = loaded
                        .state
                        .context
                        .entries
                        .iter()
                        .find(|active| active.key.as_ref() == Some(&key))
                        .filter(|active| active_context_entry_matches_input(active, &input))
                    {
                        let effective = active_entry_input(active);
                        let text = text.filter(|_| effective.content_ref == input.content_ref);
                        ordered.push(PreparedAppend::Ready {
                            key,
                            input: effective,
                            text,
                        });
                    } else {
                        ordered.push(PreparedAppend::Ready {
                            key: key.clone(),
                            input: input.clone(),
                            text,
                        });
                        pending.push((key, input));
                    }
                }
            }
        }
        let (context_revision, outcomes) = if pending.is_empty() {
            (loaded.state.context.revision, BTreeMap::new())
        } else {
            let correlations = self
                .submit_correlated_context_commands(
                    &session_id,
                    pending
                        .iter()
                        .map(|(key, entry)| CoreAgentCommand::UpsertContext {
                            expected_revision: None,
                            key: key.clone(),
                            entry: entry.clone(),
                        })
                        .collect(),
                )
                .await?;
            self.wait_for_context_append_outcomes(&session_id, &pending, &correlations)
                .await?
        };
        let mut response_results = Vec::with_capacity(ordered.len());
        for item in ordered {
            match item {
                PreparedAppend::Failed(result) => response_results.push(result),
                PreparedAppend::Ready { key, input, text } => {
                    let result = match outcomes.get(&key) {
                        Some(ContextAppendWaitOutcome::Applied { entry }) => {
                            let text = text
                                .as_deref()
                                .filter(|_| entry.content_ref == input.content_ref);
                            context_append_result(
                                self.store.as_ref(),
                                key.as_str().to_owned(),
                                ContextAppendStatus::Applied,
                                entry,
                                text,
                            )
                            .await?
                        }
                        Some(ContextAppendWaitOutcome::Failed { failure }) => {
                            context_append_failed_result(
                                key.as_str().to_owned(),
                                input_admission_failure_from_workflow(failure),
                            )
                        }
                        None => {
                            context_append_result(
                                self.store.as_ref(),
                                key.as_str().to_owned(),
                                ContextAppendStatus::Unchanged,
                                &input,
                                text.as_deref(),
                            )
                            .await?
                        }
                    };
                    response_results.push(result);
                }
            }
        }
        Ok(AgentApiOutcome::new(ContextAppendResponse {
            context_revision,
            results: response_results,
        }))
    }

    async fn remove_context(
        &self,
        params: ContextRemoveParams,
    ) -> Result<AgentApiOutcome<ContextRemoveResponse>, AgentApiError> {
        const MAX_CONTEXT_REMOVE_KEYS: usize = 64;

        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        if params.keys.is_empty() {
            return Err(AgentApiError::invalid_request(
                "session/context/remove requires at least one key",
            ));
        }
        if params.keys.len() > MAX_CONTEXT_REMOVE_KEYS {
            return Err(AgentApiError::invalid_request(format!(
                "session/context/remove accepts at most {MAX_CONTEXT_REMOVE_KEYS} keys per call"
            )));
        }
        let mut keys = Vec::with_capacity(params.keys.len());
        let mut seen_keys = BTreeSet::new();
        for key in params.keys {
            let key = ContextEntryKey::try_new(key).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid context key: {error}"))
            })?;
            engine::validate_external_context_key(&key).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid context key: {error}"))
            })?;
            if !seen_keys.insert(key.clone()) {
                return Err(AgentApiError::invalid_request(format!(
                    "duplicate context key in remove batch: {key}"
                )));
            }
            keys.push(key);
        }

        let loaded = self.load_session_state(&session_id).await?;
        if loaded.state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        let mut pending = Vec::new();
        let mut absent = BTreeSet::new();
        for key in &keys {
            let present = loaded
                .state
                .context
                .entries
                .iter()
                .any(|entry| entry.key.as_ref() == Some(key));
            if present {
                pending.push(key.clone());
            } else {
                absent.insert(key.clone());
            }
        }
        let (context_revision, outcomes) = if pending.is_empty() {
            (loaded.state.context.revision, BTreeMap::new())
        } else {
            let correlations = self
                .submit_correlated_context_commands(
                    &session_id,
                    pending
                        .iter()
                        .map(|key| CoreAgentCommand::RemoveContext {
                            expected_revision: None,
                            key: key.clone(),
                        })
                        .collect(),
                )
                .await?;
            self.wait_for_context_keys_removed(&session_id, &pending, &correlations)
                .await?
        };
        let results = keys
            .into_iter()
            .map(|key| {
                if absent.contains(&key) {
                    return ContextRemoveResult {
                        key: key.as_str().to_owned(),
                        status: ContextRemoveStatus::Absent,
                        failure: None,
                    };
                }
                match outcomes.get(&key) {
                    Some(Some(failure)) => ContextRemoveResult {
                        key: key.as_str().to_owned(),
                        status: ContextRemoveStatus::Failed,
                        failure: Some(input_admission_failure_from_workflow(failure)),
                    },
                    _ => ContextRemoveResult {
                        key: key.as_str().to_owned(),
                        status: ContextRemoveStatus::Removed,
                        failure: None,
                    },
                }
            })
            .collect();
        Ok(AgentApiOutcome::new(ContextRemoveResponse {
            context_revision,
            results,
        }))
    }

    async fn start_run(
        &self,
        params: RunStartParams,
    ) -> Result<AgentApiOutcome<RunStartResponse>, AgentApiError> {
        self.start_run_internal(params, Vec::new()).await
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
                if active.run_id == requested_run_id
                    && matches!(
                        active.status,
                        RunStatus::Active
                            | RunStatus::Parked
                            | RunStatus::Cancelling
                            | RunStatus::CancellingGrace
                    ) => {}
            Some(active) if active.run_id == requested_run_id => {
                return Err(AgentApiError::rejected(format!(
                    "run is not cancellable: {}",
                    params.run_id
                )));
            }
            _ if loaded
                .state
                .runs
                .queued
                .iter()
                .any(|run| run.run_id == requested_run_id) => {}
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
        self.submit_core_command(
            &session_id,
            CoreAgentCommand::CancelRun {
                run_id: requested_run_id,
            },
        )
        .await?;
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
            catalog.catalog_id.clone(),
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
                expected_revision: None,
                key: skill_activation_context_key(&catalog.catalog_id, &skill_id),
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
                catalog_id: catalog.catalog_id.clone(),
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
                expected_revision: None,
                key: skill_activation_context_key(tools::skills::VFS_SKILL_CATALOG_ID, &skill_id),
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

    async fn create_environment(
        &self,
        params: EnvironmentCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentCreateResponse>, AgentApiError> {
        self.create_environment_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn read_environment(
        &self,
        params: EnvironmentReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentReadResponse>, AgentApiError> {
        self.read_environment_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn list_environments(
        &self,
        params: EnvironmentListParams,
    ) -> Result<AgentApiOutcome<EnvironmentListResponse>, AgentApiError> {
        self.list_environment_records(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn close_environment(
        &self,
        params: EnvironmentCloseParams,
    ) -> Result<AgentApiOutcome<EnvironmentCloseResponse>, AgentApiError> {
        self.close_environment_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn activate_session_environment(
        &self,
        params: SessionEnvironmentActivateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentActivateResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let loaded = self.load_session_state(&session_id).await?;
        self.require_open_idle_session(&session_id, &loaded, "environment activation")?;
        self.selectable_environment_for_session(&loaded.state, &environment_id)
            .await?;

        if loaded.state.environment.active_environment_id.as_ref() != Some(&environment_id) {
            let baseline_failures = self
                .query_status_optional(&session_id)
                .await?
                .map(|status| status.admission_failures.len())
                .unwrap_or(0);
            self.submit_core_command(
                &session_id,
                activate_environment_command(environment_id.clone()),
            )
            .await?;
            self.wait_for_active_environment(&session_id, Some(&environment_id), baseline_failures)
                .await?;
        }
        Ok(AgentApiOutcome::new(SessionEnvironmentActivateResponse {
            session: self.project_session_by_id(&session_id).await?,
        }))
    }

    async fn deactivate_session_environment(
        &self,
        params: SessionEnvironmentDeactivateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentDeactivateResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let loaded = self.load_session_state(&session_id).await?;
        self.require_open_idle_session(&session_id, &loaded, "environment deactivation")?;

        if loaded.state.environment.active_environment_id.is_some() {
            let baseline_failures = self
                .query_status_optional(&session_id)
                .await?
                .map(|status| status.admission_failures.len())
                .unwrap_or(0);
            self.submit_core_command(&session_id, deactivate_environment_command())
                .await?;
            self.wait_for_active_environment(&session_id, None, baseline_failures)
                .await?;
        }
        Ok(AgentApiOutcome::new(SessionEnvironmentDeactivateResponse {
            session: self.project_session_by_id(&session_id).await?,
        }))
    }

    async fn bind_environment_credential(
        &self,
        params: EnvironmentCredentialBindParams,
    ) -> Result<AgentApiOutcome<EnvironmentCredentialBindResponse>, AgentApiError> {
        self.bind_environment_credential_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn list_environment_credentials(
        &self,
        params: EnvironmentCredentialListParams,
    ) -> Result<AgentApiOutcome<EnvironmentCredentialListResponse>, AgentApiError> {
        self.list_environment_credential_records(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn unbind_environment_credential(
        &self,
        params: EnvironmentCredentialUnbindParams,
    ) -> Result<AgentApiOutcome<EnvironmentCredentialUnbindResponse>, AgentApiError> {
        self.unbind_environment_credential_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn create_environment_jobs(
        &self,
        params: EnvironmentJobCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobCreateResponse>, AgentApiError> {
        self.create_environment_job_records(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn read_environment_jobs(
        &self,
        params: EnvironmentJobReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobReadResponse>, AgentApiError> {
        self.read_environment_job_records(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn cancel_environment_jobs(
        &self,
        params: EnvironmentJobCancelParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobCancelResponse>, AgentApiError> {
        self.cancel_environment_job_records(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn list_environment_provider_bindings(
        &self,
        params: EnvironmentProviderBindingListParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderBindingListResponse>, AgentApiError> {
        self.list_environment_provider_binding_records(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn read_environment_provider_binding(
        &self,
        params: EnvironmentProviderBindingReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderBindingReadResponse>, AgentApiError> {
        self.read_environment_provider_binding_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn list_environment_templates(
        &self,
        params: EnvironmentTemplateListParams,
    ) -> Result<AgentApiOutcome<EnvironmentTemplateListResponse>, AgentApiError> {
        self.list_environment_template_records(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn read_environment_template(
        &self,
        params: EnvironmentTemplateReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentTemplateReadResponse>, AgentApiError> {
        self.read_environment_template_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn put_blobs(
        &self,
        params: BlobPutParams,
    ) -> Result<AgentApiOutcome<BlobPutResponse>, AgentApiError> {
        put_blobs(self.store.as_ref(), params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn read_blob(
        &self,
        params: BlobReadParams,
    ) -> Result<AgentApiOutcome<BlobReadResponse>, AgentApiError> {
        read_blob(self.store.as_ref(), params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn has_blobs(
        &self,
        params: BlobHasParams,
    ) -> Result<AgentApiOutcome<BlobHasResponse>, AgentApiError> {
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
            VfsSnapshotSource::new("api_commit").with_subject("vfs/snapshots/commit"),
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

    async fn list_vfs_workspaces(
        &self,
        _params: VfsWorkspaceListParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceListResponse>, AgentApiError> {
        let workspaces = self.list_vfs_workspace_records().await?;
        Ok(AgentApiOutcome::new(VfsWorkspaceListResponse {
            workspaces: workspaces.into_iter().map(vfs_workspace_view).collect(),
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

    async fn put_mcp_server(
        &self,
        params: McpServerPutParams,
    ) -> Result<AgentApiOutcome<McpServerPutResponse>, AgentApiError> {
        let record = put_mcp_server_record(params.server, now_ms()?)?;
        let grant = match record.auth_grant_id.as_ref() {
            Some(grant_id) => Some(
                self.store
                    .read_grant(grant_id)
                    .await
                    .map_err(map_auth_error)?,
            ),
            None => None,
        };
        mcp_api::validate_mcp_server_credential(&record, grant.as_ref())?;
        let server = self
            .store
            .put_server(record, params.expected_revision)
            .await
            .map_err(map_mcp_error)?;
        Ok(AgentApiOutcome::new(McpServerPutResponse {
            server: mcp_server_view(server),
        }))
    }

    async fn list_mcp_servers(
        &self,
        params: McpServerListParams,
    ) -> Result<AgentApiOutcome<McpServerListResponse>, AgentApiError> {
        let servers = self
            .store
            .list_servers(mcp::ListMcpServers {
                status: params.status.map(mcp_api::registry_status_for_filter),
            })
            .await
            .map_err(map_mcp_error)?
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
            .map_err(map_mcp_error)?;
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
            .map_err(map_mcp_error)?;
        Ok(AgentApiOutcome::new(McpServerDeleteResponse {
            server: mcp_server_view(server),
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
            .map_err(map_auth_error)?;
        match self.store.create_grant(draft.grant).await {
            Ok(record) => Ok(AgentApiOutcome::new(AuthGrantImportResponse {
                grant: auth_grant_view(record),
            })),
            Err(error) => {
                // The secret is orphaned without its grant; clean up best-effort
                // so a failed import does not leave sealed values behind.
                let _ = self.store.delete_secret(&draft.secret.secret_id).await;
                Err(map_auth_error(error))
            }
        }
    }

    async fn list_auth_grants(
        &self,
        params: AuthGrantListParams,
    ) -> Result<AgentApiOutcome<AuthGrantListResponse>, AgentApiError> {
        let grants = self
            .store
            .list_grants(auth::ListAuthGrants {
                status: params.status.map(registry_auth_grant_status_for_filter),
            })
            .await
            .map_err(map_auth_error)?;
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
            .map_err(map_auth_error)?;
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
            .update_grant_status(&grant_id, auth::AuthGrantStatus::Revoked, now_ms()?)
            .await
            .map_err(map_auth_error)?;
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
                .map_err(map_auth_error)?;
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
                Err(map_auth_error(error))
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
            .map_err(map_auth_error)?;
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
            .map_err(map_auth_error)?;
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
            .map_err(map_auth_error)?;
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
                principal: crate::gateway::principal::request_principal(),
            })
            .await
            .map_err(map_auth_error)?;
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
            .map_err(map_auth_error)?;
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
        if let auth::AuthProviderConfig::ModelOAuth(config) = &draft.provider.config {
            let grant = self
                .store
                .read_grant(&config.grant_id)
                .await
                .map_err(map_auth_error)?;
            if grant.status != auth::AuthGrantStatus::Active {
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
                .map_err(map_auth_error)?;
        }
        match self.store.create_auth_provider(draft.provider).await {
            Ok(record) => Ok(AgentApiOutcome::new(AuthProviderCreateResponse {
                provider: auth_provider_view(record),
            })),
            Err(error) => {
                if let Some(secret) = &draft.secret {
                    let _ = self.store.delete_secret(&secret.secret_id).await;
                }
                Err(map_auth_error(error))
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
            .map_err(map_auth_error)?;
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
            .map_err(map_auth_error)?;
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
            .map_err(map_auth_error)?;
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
        let auth::AuthProviderConfig::GitHubApp(config) = &provider.config else {
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
            installations: installations.iter().map(github_installation_view).collect(),
        }))
    }

    async fn grant_github_installation(
        &self,
        params: AuthGitHubInstallationGrantParams,
    ) -> Result<AgentApiOutcome<AuthGitHubInstallationGrantResponse>, AgentApiError> {
        let (provider, app_jwt) = self.github_provider_jwt(params.provider_id).await?;
        let auth::AuthProviderConfig::GitHubApp(config) = &provider.config else {
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
            .map_err(map_auth_error)?;
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
    ) -> Result<auth::OAuthClientId, AgentApiError> {
        // A manually registered `mcp:<server_id>` client always wins: reuse
        // it without touching the catalog or the network, so login works
        // even when the catalog record is named differently or absent.
        let client_id = auth::mcp_oauth_client_id(server_id).map_err(map_auth_error)?;
        match self.store.read_oauth_client(&client_id).await {
            Ok(existing) => return Ok(existing.client_id),
            Err(auth::AuthRegistryError::ClientNotFound { .. }) => {}
            Err(error) => return Err(map_auth_error(error)),
        }

        let server_id = parse_mcp_server_id(server_id.to_owned())?;
        let record = self
            .store
            .read_server(&server_id)
            .await
            .map_err(map_mcp_error)?;
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

    pub fn public_base_url(&self) -> &str {
        &self.public_base_url
    }

    /// Load a GitHub App provider and sign its app JWT for control-plane
    /// calls (installation listing/verification). The JWT and the key only
    /// exist in memory inside [`auth::SecretValue`] wrappers.
    async fn github_provider_jwt(
        &self,
        provider_id: String,
    ) -> Result<(auth::AuthProviderRecord, auth::SecretValue), AgentApiError> {
        let provider_id = parse_auth_provider_id(provider_id)?;
        let provider = self
            .store
            .read_auth_provider(&provider_id)
            .await
            .map_err(map_auth_error)?;
        let auth::AuthProviderConfig::GitHubApp(config) = &provider.config else {
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
            .map_err(map_auth_error)?;
        let app_jwt = auth::sign_github_app_jwt(&config.app_id, &private_key, now_ms()?)
            .map_err(map_github_app_error)?;
        Ok((provider, app_jwt))
    }

    /// Handle the OAuth redirect: consume the flow, exchange the code, and
    /// store the resulting grant. Called by the gateway's HTTP callback
    /// route, not via JSON-RPC.
    pub async fn complete_oauth_callback(
        &self,
        callback: auth::AuthCallback,
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
                message: map_auth_error(error).message,
            },
        }
    }
}
#[cfg(test)]
mod tests;

/// Deployment-scoped CIMD document: depends only on the public base URL, so
/// the multi-universe HTTP edge serves it without resolving a universe.
pub(crate) fn cimd_document_for(public_base_url: &str) -> serde_json::Value {
    oauth_api::cimd_document(public_base_url)
}
