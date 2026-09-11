//! `api` gateway for the Temporal-backed agent workflow.

mod api_config;
mod auth_api;
mod blobs;
mod bots_api;
mod catalogs;
pub(crate) mod channels_api;
mod common;
mod environment_credentials;
mod environment_lifecycle;
mod environment_power;
mod environment_projection;
pub(crate) mod environment_providers;
mod environment_registration;
mod environments;
mod errors;
mod event_history;
mod github_api;
mod input;
mod instructions;
mod mcp_api;
pub(crate) mod mcp_discovery;
mod models_api;
mod oauth_api;
mod parse;
mod profiles;
mod prompts;
mod provider_controllers;
mod session_jobs;
mod session_toolset;
mod skills;
mod subagents_api;
mod vfs_api;
mod workflow;

use api_config::engine_session_config_from_api;
#[cfg(test)]
use api_config::*;
use auth_api::{
    api_auth_provider_kind, auth_grant_import_draft, auth_grant_view, map_auth_broker_error,
    map_auth_error, parse_auth_grant_id, registry_auth_grant_exposure,
    registry_auth_grant_status_for_filter, require_retrievable_grant,
};
use blobs::{has_blobs, put_blobs, read_blob};
use catalogs::parse_client_context_key;
use common::now_ms;
pub use environment_lifecycle::ReconcileFailureLog;
use environment_lifecycle::parse_registry_environment_id;
pub use environment_power::PowerReaperStats;
use environment_providers::{map_environments_error, parse_environment_provider_id};
use environments::{activate_environment_command, deactivate_environment_command};
use errors::*;
use github_api::{
    auth_provider_create_draft, auth_provider_view, github_installation_grant_draft,
    github_installation_view, map_github_app_error, parse_auth_provider_id,
};
use input::{context_entry_input_from_api, run_input_from_api};
use mcp_api::{map_mcp_error, mcp_server_view, parse_mcp_server_id, put_mcp_server_record};
use mcp_discovery::{
    ConfiguratorTrustedHeaderPolicy, HttpMcpToolDiscoverer, McpDiscoveryGate, McpToolDiscoverer,
};
use models_api::{ModelDiscoveryService, stored_provider_key_resolver};
use oauth_api::{
    auth_client_create_draft, auth_flow_view, cimd_config, map_mcp_oauth_error,
    mcp_oauth_target_from_record, oauth_client_view, oauth_redirect_uri, parse_auth_flow_id,
    parse_oauth_client_id,
};
use parse::*;
use provider_controllers::{
    ProviderControllerConnector, WebSocketProviderControllerConnector, finish_provider_controller,
};
#[cfg(test)]
use skills::skill_list_response;
#[cfg(test)]
use tools::skills::SkillMetadata;
use vfs_api::{commit_vfs_snapshot, read_vfs_snapshot, vfs_workspace_view};

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    sync::Arc,
    time::{Duration, Instant},
};

use api::*;
use api_projection::{
    CoreAgentProjector, ProjectRun, ProjectSession, api_kind_from_str, api_run_id, api_steering_id,
    core_run_status_to_api_status, decode_stored_entry, event_cursor, event_page_limit,
    map_session_store_error, parse_api_run_id, project_context_entry_inputs,
};
use async_trait::async_trait;
use auth::{
    AuthFlowStore, AuthGrantStore, AuthProviderStore, AuthTokenBroker, GitHubApiClient,
    GitHubAppRuntime, GrantRefreshLock, HttpGitHubApiClient, HttpOAuthMetadataClient,
    HttpOAuthTokenClient, McpOAuthDriver, OAuthClientStore, OAuthFlowService, OAuthMetadataClient,
    OAuthRefreshRuntime, OAuthTokenClient, RegistryTokenBroker, SecretStore, StartAuthFlow,
    TokenAudience,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use engine::{
    ApprovalId, BlobRef, BoundWorkflowToolDispatch, CompactionPolicy, ContextEntry,
    ContextEntryInput, ContextEntryKey, ContextEntryKind, ContextMessageRole, CoreAgentCommand,
    CoreAgentStatus, FunctionToolSpec, ManagedSessionWorkflowTools, ModelSelection,
    ProviderApiKind, RunConfig, RunId, RunStatus, SessionConfig, SessionId, SubmissionId,
    ToolChoice, ToolKind, ToolName, ToolParallelism, ToolSpec, WorkflowEndpointRef,
    WorkflowStartRef, WorkflowToolCompletion, WorkflowToolCompletionKeySource,
    WorkflowToolDeclaration, WorkflowToolDefinition, WorkflowToolId, WorkflowToolTarget,
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
    catalog::{SKILL_CATALOG_CONTEXT_KEY, SUBAGENT_CATALOG_CONTEXT_KEY, VFS_CATALOG_CONTEXT_KEY},
    environment::jobs::{
        JOB_RUN_DEADLINE_AFTER_MS, JOB_RUN_WORKFLOW_SEMANTIC_TYPE, JOB_RUN_WORKFLOW_TOOL_ID,
        JOB_SUBMIT_WORKFLOW_SEMANTIC_TYPE, JOB_SUBMIT_WORKFLOW_TOOL_ID,
    },
    skills::{
        SkillCatalogSnapshot, SkillLocation, configured_vfs_skill_root_specs,
        resolve_linked_vfs_skill_roots,
    },
    toolset::{
        RegisteredToolset, ToolsetConfig, enable_concurrency_for_workflow_tools, register_toolset,
        register_workflow_tools,
    },
    web::search::WebSearchToolConfig,
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
const DEFAULT_RUN_SUMMARY_LIMIT: usize = 20;
const MAX_RUN_SUMMARY_LIMIT: usize = 100;
const MAX_RUN_DETAIL_LIMIT: usize = 512;
/// Hard ceiling on the events scanned while projecting one run's complete
/// interval; a run past this is served through `session/events/read` instead
/// of an unbounded detail document.
const MAX_RUN_DETAIL_EVENTS: usize = 20_000;
/// Resource bound for the opaque managed-controller run terminal token.
const MAX_RUN_TERMINAL_NOTIFICATION_TOKEN_BYTES: usize = 512;

/// Default public base URL for the gateway-hosted OAuth callback; matches
/// `DEFAULT_GATEWAY_BIND`. Hosted deployments must set the real public URL.
pub const DEFAULT_PUBLIC_BASE_URL: &str = "http://127.0.0.1:18080";

fn env_flag(name: &str) -> bool {
    env::var(name)
        .ok()
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

/// Caller-supplied metadata on sessions and environments shares one validator
/// (bounds plus the reserved-prefix rule), so a session and the environment
/// it ran in accept the same keys.
pub(super) fn validate_caller_metadata(
    metadata: &BTreeMap<String, String>,
) -> Result<(), AgentApiError> {
    environment_protocol::registration::validate_registration_metadata(None, metadata)
        .map_err(|message| AgentApiError::invalid_request(format!("invalid metadata: {message}")))
}

fn validate_delete_after_close_ms(value: Option<u64>) -> Result<(), AgentApiError> {
    if let Some(value) = value
        && !(1..=MAX_SESSION_DELETE_AFTER_CLOSE_MS).contains(&value)
    {
        return Err(AgentApiError::invalid_request(format!(
            "deleteAfterCloseMs must be 1..={MAX_SESSION_DELETE_AFTER_CLOSE_MS}"
        )));
    }
    Ok(())
}

fn session_retention_view(
    record: &engine::storage::SessionRecord,
    root: &engine::storage::SessionRecord,
) -> SessionRetentionView {
    SessionRetentionView {
        root_session_id: record.retention_root_session_id.as_str().to_owned(),
        delete_after_close_ms: root.delete_after_close_ms,
        delete_at_ms: root.delete_at_ms,
    }
}

fn session_summary_view(
    record: engine::storage::SessionRecord,
    root: &engine::storage::SessionRecord,
) -> SessionSummaryView {
    let retention = session_retention_view(&record, root);
    SessionSummaryView {
        id: record.session_id.as_str().to_owned(),
        display_name: record.display_name,
        metadata: record.metadata,
        lifecycle_status: match record.lifecycle_status {
            engine::storage::SessionLifecycleStatus::New => SessionLifecycleStatus::New,
            engine::storage::SessionLifecycleStatus::Open => SessionLifecycleStatus::Open,
            engine::storage::SessionLifecycleStatus::Closed => SessionLifecycleStatus::Closed,
        },
        closed_at_ms: record.closed_at_ms,
        retention,
        managed: record.managed,
        origin: record
            .origin
            .as_ref()
            .map(api_projection::session_origin_to_api)
            .transpose()
            .ok()
            .flatten(),
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
}

fn approval_decision_failure(
    approval_id: String,
    kind: ApprovalDecisionFailureKind,
    message: impl Into<String>,
) -> ApprovalDecisionResult {
    ApprovalDecisionResult {
        approval_id,
        status: ApprovalDecisionStatus::Failed,
        failure: Some(ApprovalDecisionFailure {
            kind,
            message: message.into(),
        }),
    }
}

enum ExistingRunSubmission {
    ReturnRun { run_id: RunId },
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
            if active.source.matches_request(source)
                && &active.run_config == run_config
                && active.notify_on_terminal == notify_on_terminal
            {
                ExistingRunSubmission::ReturnRun {
                    run_id: active.run_id,
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
        if !queued.source.matches_request(source)
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
            },
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
        api_projection::project_content_text(store, &input.content).await?
    } else if context_append_entry_has_activation_text(input) {
        // The submitted text is reused when it produced this exact entry so
        // plain-text appends do not pay a blob read per response entry.
        match submitted_text {
            Some(text) => Some(text.to_owned()),
            None => Some(
                store
                    .read_text(&input.content.content_ref)
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
            .content
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
        content: entry.content.clone(),
        preview: entry.preview.clone(),
        origin: entry.origin.clone(),
        provenance_ref: entry.provenance_ref.clone(),
        token_estimate: entry.token_estimate.clone(),
    }
}

fn active_context_entry_matches_input(active: &ContextEntry, input: &ContextEntryInput) -> bool {
    let active_input = active_entry_input(active);
    active_input == *input || audio_input_matches_transcript(input, &active_input)
}

fn audio_input_matches_transcript(input: &ContextEntryInput, active: &ContextEntryInput) -> bool {
    input
        .content
        .media_type
        .as_deref()
        .is_some_and(|mime| mime.trim().to_ascii_lowercase().starts_with("audio/"))
        && is_audio_transcript_entry(active)
        && active.provenance_ref.as_ref() == Some(&input.content.content_ref)
}

fn is_audio_transcript_entry(input: &ContextEntryInput) -> bool {
    input.content.provider_kind.as_deref()
        == Some(llm_clients::content::AUDIO_TRANSCRIPT_PROVIDER_KIND)
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
    bot_task_queue: String,
    channel_task_queue: String,
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
    provider_controller_connector: Arc<dyn ProviderControllerConnector>,
    environment_gateway: crate::environment_gateway::EnvironmentGatewayClientConfig,
}

impl GatewayAgentApiBuilder {
    pub fn with_task_queue(mut self, task_queue: impl Into<String>) -> Self {
        self.task_queue = task_queue.into();
        self
    }

    /// Task queue of the `bots` worker role (bot controllers, trigger fires).
    pub fn with_bot_task_queue(mut self, task_queue: impl Into<String>) -> Self {
        self.bot_task_queue = task_queue.into();
        self
    }

    /// Task queue of the `channels` worker role (conversation workflows).
    pub fn with_channel_task_queue(mut self, task_queue: impl Into<String>) -> Self {
        self.channel_task_queue = task_queue.into();
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
    pub(crate) fn with_provider_controller_connector(
        mut self,
        connector: Arc<dyn ProviderControllerConnector>,
    ) -> Self {
        self.provider_controller_connector = connector;
        self
    }

    pub fn with_default_model(mut self, model: ModelSelection) -> Self {
        self.default_model = model;
        self
    }

    pub fn with_environment_gateway(
        mut self,
        gateway: crate::environment_gateway::EnvironmentGatewayClientConfig,
    ) -> Self {
        self.environment_gateway = gateway;
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
        let allow_private_mcp = env_flag("LIGHTSPEED_MCP_OAUTH_ALLOW_PRIVATE_NETWORKS");
        let mcp_private_networks = crate::worker::mcp::McpPrivateNetworkPolicy::from_env()
            .expect("parse LIGHTSPEED_MCP_PRIVATE_NETWORKS");
        let configurator_trusted_header = ConfiguratorTrustedHeaderPolicy::from_env()
            .expect("parse LIGHTSPEED_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER_URL");
        let metadata_client = self.oauth_metadata_client.unwrap_or_else(|| {
            Arc::new(HttpOAuthMetadataClient::with_private_networks(
                allow_private_mcp,
            ))
        });
        let token_client = self.oauth_token_client.unwrap_or_else(|| {
            Arc::new(
                HttpOAuthTokenClient::new_with_mcp_http(metadata_client.clone())
                    .expect("construct OAuth token endpoint HTTP client"),
            )
        });
        let oauth_flows = OAuthFlowService::new(
            self.store.clone() as Arc<dyn OAuthClientStore>,
            self.store.clone() as Arc<dyn AuthFlowStore>,
            self.store.clone() as Arc<dyn AuthGrantStore>,
            self.store.clone() as Arc<dyn SecretStore>,
            token_client.clone(),
        );
        let mcp_oauth = McpOAuthDriver::new(
            self.store.clone() as Arc<dyn OAuthClientStore>,
            self.store.clone() as Arc<dyn SecretStore>,
            metadata_client,
        );
        let mcp_tool_discoverer: Arc<dyn McpToolDiscoverer> =
            Arc::new(HttpMcpToolDiscoverer::new());
        let mcp_discovery_gate = Arc::new(McpDiscoveryGate::new(Duration::from_secs(2)));
        let github_api = self.github_api_client.unwrap_or_else(|| {
            Arc::new(HttpGitHubApiClient::new().expect("construct GitHub REST HTTP client"))
        });
        let grants: Arc<dyn AuthGrantStore> = self.store.clone();
        let secrets: Arc<dyn SecretStore> = self.store.clone();
        let providers: Arc<dyn AuthProviderStore> = self.store.clone();
        let auth_token_broker: Arc<dyn AuthTokenBroker> = Arc::new(
            RegistryTokenBroker::new(
                grants.clone(),
                secrets.clone(),
                self.store.clone() as Arc<dyn GrantRefreshLock>,
            )
            .with_oauth_refresh(OAuthRefreshRuntime::new(
                self.store.clone() as Arc<dyn OAuthClientStore>,
                token_client.clone(),
            ))
            .with_token_source(
                auth::AuthProviderKind::GitHubApp,
                Arc::new(GitHubAppRuntime::new(
                    providers,
                    github_api.clone(),
                    grants,
                    secrets,
                )),
            ),
        );
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
            self.store.clone() as Arc<dyn AuthProviderStore>,
        );
        GatewayAgentApi {
            client: self.client,
            store: self.store,
            task_queue: self.task_queue,
            bot_task_queue: self.bot_task_queue,
            channel_task_queue: self.channel_task_queue,
            default_model: self.default_model,
            continue_as_new_history_threshold: self.continue_as_new_history_threshold,
            poll_interval: self.poll_interval,
            operation_timeout: self.operation_timeout,
            events_wait_cap: self.events_wait_cap,
            public_base_url: self.public_base_url,
            oauth_flows,
            auth_token_broker,
            mcp_oauth,
            mcp_tool_discoverer,
            mcp_private_networks,
            configurator_trusted_header,
            mcp_discovery_gate,
            github_api,
            model_discovery,
            provider_controller_connector: self.provider_controller_connector,
            environment_gateway: self.environment_gateway,
        }
    }
}

pub struct GatewayAgentApi {
    client: Client,
    store: Arc<PgStore>,
    task_queue: String,
    pub(crate) bot_task_queue: String,
    pub(crate) channel_task_queue: String,
    default_model: ModelSelection,
    continue_as_new_history_threshold: Option<u32>,
    poll_interval: Duration,
    operation_timeout: Duration,
    events_wait_cap: Duration,
    public_base_url: String,
    oauth_flows: OAuthFlowService,
    auth_token_broker: Arc<dyn AuthTokenBroker>,
    mcp_oauth: McpOAuthDriver,
    mcp_tool_discoverer: Arc<dyn McpToolDiscoverer>,
    mcp_private_networks: crate::worker::mcp::McpPrivateNetworkPolicy,
    configurator_trusted_header: ConfiguratorTrustedHeaderPolicy,
    mcp_discovery_gate: Arc<McpDiscoveryGate>,
    github_api: Arc<dyn GitHubApiClient>,
    model_discovery: ModelDiscoveryService,
    provider_controller_connector: Arc<dyn ProviderControllerConnector>,
    pub(crate) environment_gateway: crate::environment_gateway::EnvironmentGatewayClientConfig,
}

impl GatewayAgentApi {
    pub fn builder(client: Client, store: Arc<PgStore>) -> GatewayAgentApiBuilder {
        let environment_gateway = crate::environment_gateway::EnvironmentGatewayClientConfig::new(
            DEFAULT_PUBLIC_BASE_URL,
            format!("local-{}", uuid::Uuid::new_v4()),
        );
        GatewayAgentApiBuilder {
            client,
            store,
            task_queue: DEFAULT_TASK_QUEUE.to_owned(),
            bot_task_queue: temporal_workflow::bots::DEFAULT_BOTS_TASK_QUEUE.to_owned(),
            channel_task_queue: crate::config::DEFAULT_CHANNELS_TASK_QUEUE.to_owned(),
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
            provider_controller_connector: Arc::new(WebSocketProviderControllerConnector::default()),
            environment_gateway,
        }
    }

    pub(crate) fn store(&self) -> &Arc<PgStore> {
        &self.store
    }

    pub(crate) fn temporal_client(&self) -> &Client {
        &self.client
    }

    /// Task queue of the `bots` worker role.
    pub(crate) fn bot_task_queue(&self) -> &str {
        &self.bot_task_queue
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
    pub(crate) fn session_toolset_config(
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
            if let Some(search) = &web.search {
                config.web.search = Some(WebSearchToolConfig::new(
                    search.allowed_domains.clone().unwrap_or_default(),
                    search.blocked_domains.clone(),
                ));
            }
            if web.fetch.is_some() {
                config.web.fetch = true;
            }
        }
        if features.timers.is_some() || features.subagents.is_some() {
            // Joining spawned sub-agents depends on the base concurrency
            // tools, so the subagents grant implies them; the timers grant
            // adds nothing extra today beyond the same surface.
            config.concurrency = tools::concurrency::ConcurrencyToolsetConfig::timer();
        }
        if include_environment_tools && let Some(environment) = &features.environments {
            config.builtin.environment.filesystem = match environment.tools {
                None => tools::toolset::FilesystemToolsetConfig::disabled(),
                Some(engine::EnvironmentToolSurface::ReadOnly) => {
                    tools::toolset::FilesystemToolsetConfig::read_only()
                }
                Some(engine::EnvironmentToolSurface::Edit) => {
                    tools::toolset::FilesystemToolsetConfig::workspace_edit()
                }
            };
            config.builtin.environment.run_process = environment.commands;
            config.builtin.environment.continue_process = environment.commands;
        }
        if include_job_read_tool {
            config.builtin.environment.job_read = true;
        }
        config
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

    /// Sub-agent run start: identical to the public `session/runs/start`
    /// boundary except that the admitted `RunRequestCommand` carries the
    /// execution's cross-session notify-intent. Public callers can request
    /// only the single destination derived from the durable lifecycle
    /// controller.
    pub(crate) async fn start_run_for_subagent(
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
        let RunStartSource::Input { items } = source;
        let source = engine::RunRequestSource::Input {
            input: run_input_from_api(self.store.as_ref(), &items).await?,
        };
        if let Some(existing) = existing_run_submission(
            &loaded.state,
            &submission_id,
            &source,
            &run_config,
            &notify_on_terminal,
        ) {
            return match existing {
                ExistingRunSubmission::ReturnRun { run_id } => {
                    let run = self.project_run_by_id(&session_id, run_id).await?;
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
        // MCP server records are universe-owned mutable policy. Reconcile the
        // linked records before every new run so exposure and allowlist edits
        // do not remain pinned to the session's previous materialization. A
        // tool patch cannot move the revision of a request already in flight,
        // so signal it first and let the workflow apply it at the next turn
        // boundary before the subsequently queued run uses the toolset.
        let turn_in_flight = loaded
            .state
            .runs
            .active
            .as_ref()
            .is_some_and(|run| run.active_turn_id.is_some());
        let _ = self
            .configure_session_toolset(&session_id, &loaded, !turn_in_flight)
            .await?;
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
                    let session = self.session_mutation_view_by_id(&session_id).await?;
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
        let mut resolved_profile = match profile {
            Some(source) => Some(self.resolve_profile_source(source).await?),
            None => None,
        };
        if let Some(environment) = environment {
            let environment = match environment {
                SessionEnvironmentOverride::None {} => None,
                SessionEnvironmentOverride::Existing { environment_id } => {
                    Some(ProfileEnvironment::Existing { environment_id })
                }
            };
            ::profiles::validate_profile_document(&ProfileDocument {
                environment: environment.clone(),
                ..Default::default()
            })
            .map_err(profiles::map_profile_error)?;
            if let Some(profile) = resolved_profile.as_mut() {
                profile.document.environment = environment;
            } else if environment.is_some() {
                resolved_profile = Some(profiles::ResolvedAgentProfile {
                    profile_id: None,
                    document: ProfileDocument {
                        environment,
                        ..Default::default()
                    },
                });
            }
        }
        let effective_metadata = profiles::merge_profile_start_metadata(
            resolved_profile
                .as_ref()
                .map(|profile| &profile.document.metadata),
            metadata,
        );
        validate_caller_metadata(&effective_metadata)?;
        let effective_delete_after_close_ms = profiles::merge_profile_start_retention(
            resolved_profile
                .as_ref()
                .and_then(|profile| profile.document.retention.as_ref())
                .map(|retention| retention.delete_after_close_ms),
            delete_after_close_ms,
        );
        validate_delete_after_close_ms(effective_delete_after_close_ms)?;
        let start_config = self.merge_profile_start_config(
            resolved_profile
                .as_ref()
                .and_then(|profile| profile.document.config.clone()),
            config,
        );
        let session_config = self.session_config_for_start(start_config).await?;
        if let Some(ProfileEnvironment::Provision {
            provider_id,
            credentials,
            ..
        }) = resolved_profile
            .as_ref()
            .and_then(|profile| profile.document.environment.as_ref())
        {
            // Fail the common misconfigurations before a session or a VM
            // exists: the universe needs an enabled binding for the provider,
            // the requested credentials must resolve here, and the effective
            // config must let the session use it.
            self.resolve_profile_provision_binding(provider_id).await?;
            self.validate_profile_environment_credentials(credentials)
                .await?;
            let feature = session_config.features.environments.as_ref().ok_or_else(|| {
                AgentApiError::rejected(
                    "profile provisions an environment but the effective session config does not grant features.environments",
                )
            })?;
            if feature
                .providers
                .as_ref()
                .is_some_and(|providers| !providers.iter().any(|id| id == provider_id))
            {
                return Err(AgentApiError::rejected(format!(
                    "profile provisions from environment provider {provider_id}, which features.environments.providers does not allow"
                )));
            }
        }
        if let Some(workflow_tools) = workflow_tools.as_ref() {
            self.validate_managed_session_materialization(&session_config, workflow_tools)
                .await?;
        }
        let args = self.workflow_args(
            session_id.clone(),
            display_name,
            effective_metadata,
            effective_delete_after_close_ms,
            session_config,
            workflow_tools.clone(),
            close_on_terminal,
            auto_reject_approvals,
        );
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
                let loaded = self.load_session_state(&session_id).await?;
                if let Some(workflow_tools) = workflow_tools.as_ref() {
                    validate_managed_session_retry(
                        &loaded.state,
                        self.universe_id(),
                        workflow_tools,
                    )?;
                }
                if loaded.state.lifecycle.status == CoreAgentStatus::Closed {
                    let session = self.session_mutation_view_by_id(&session_id).await?;
                    return Ok(AgentApiOutcome::new(SessionStartResponse { session }));
                }
                self.wait_for_open_session(&session_id).await?;
                let session = self.session_mutation_view_by_id(&session_id).await?;
                return Ok(AgentApiOutcome::new(SessionStartResponse { session }));
            }
            Err(error) => return Err(error),
        }
        self.wait_for_open_session(&session_id).await?;
        let loaded = self.load_session_state(&session_id).await?;
        if let Some(workflow_tools) = workflow_tools.as_ref() {
            validate_managed_session_retry(&loaded.state, self.universe_id(), workflow_tools)?;
        }
        let _ = self
            .configure_session_toolset(&session_id, &loaded, true)
            .await?;
        if let Some(profile) = resolved_profile {
            self.apply_profile_document(&session_id, &profile, false, None, None)
                .await?;
        }
        self.load_session_state_with_current_run_context(&session_id)
            .await?;
        let session = self.session_mutation_view_by_id(&session_id).await?;
        Ok(AgentApiOutcome::new(SessionStartResponse { session }))
    }

    async fn core_environment_job_workflow_tool_declarations(
        &self,
    ) -> Result<Vec<WorkflowToolDeclaration>, AgentApiError> {
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
                    key_source: WorkflowToolCompletionKeySource::ArrayItemField {
                        pointer: "/jobs".to_owned(),
                        field: "job_id".to_owned(),
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
            let builtin = BuiltinTool::environment_canonical(operation);
            let tool = tools::definitions::register(
                builtin.logical_id(),
                tools::definitions::BuiltinSettings {
                    presentation: tools::toolset::BuiltinToolPresentation::Canonical,
                    unscoped_paths: true,
                    ..Default::default()
                },
                builtin.parallelism(),
                builtin.execution_spec(),
            );
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

    async fn core_subagent_workflow_tool_declarations(
        &self,
    ) -> Result<Vec<WorkflowToolDeclaration>, AgentApiError> {
        let recipe_bytes = serde_json::to_vec(&temporal_workflow::WorkflowToolRecipeV1 {
            workflow_type: tools::subagents::SUBAGENT_WORKFLOW_TYPE.to_owned(),
            task_queue: self.task_queue.clone(),
        })
        .map_err(|error| {
            AgentApiError::internal(format!("encode core subagent workflow recipe: {error}"))
        })?;
        let recipe_fingerprint = temporal_workflow::workflow_tool_recipe_fingerprint(&recipe_bytes);
        let recipe_ref = self
            .store
            .put_bytes(recipe_bytes)
            .await
            .map_err(map_blob_store_error)?;
        // The binding carries the hard ceiling; the grant's `deadlineMs` is
        // pinned per call and enforced inside the execution, so the
        // immutable binding never has to change with the grant.
        let definitions = [
            (
                tools::subagents::SubagentToolKind::Run,
                WorkflowToolCompletion::Joined {
                    reply_schema_ref: None,
                    deadline_after_ms: engine::SUBAGENT_DEADLINE_CEILING_MS,
                },
            ),
            (
                tools::subagents::SubagentToolKind::Spawn,
                WorkflowToolCompletion::Promises {
                    reply_schema_ref: None,
                    deadline_after_ms: Some(engine::SUBAGENT_DEADLINE_CEILING_MS),
                    max_promises: 1,
                    key_source: WorkflowToolCompletionKeySource::Reply,
                },
            ),
        ];
        let mut declarations = Vec::with_capacity(definitions.len());
        for (kind, completion) in definitions {
            let tool = tools::definitions::register(
                match kind {
                    tools::subagents::SubagentToolKind::Run => "subagent.run",
                    tools::subagents::SubagentToolKind::Spawn => "subagent.spawn",
                },
                Default::default(),
                engine::ToolParallelism::ParallelSafe,
                Default::default(),
            );
            declarations.push(WorkflowToolDeclaration::new(
                WorkflowToolDefinition {
                    tool_id: WorkflowToolId::new(kind.workflow_tool_id()),
                    revision: 1,
                    semantic_type: kind.semantic_type().to_owned(),
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

    async fn ensure_core_subagent_workflow_tools(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
    ) -> Result<(), AgentApiError> {
        if has_all_core_subagent_bindings(state) {
            return Ok(());
        }
        let baseline_failures = self
            .query_status_optional(session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        let declarations = self.core_subagent_workflow_tool_declarations().await?;
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
        self.wait_for_core_subagent_bindings(session_id, baseline_failures)
            .await
    }

    async fn wait_for_core_subagent_bindings(
        &self,
        session_id: &SessionId,
        baseline_failures: usize,
    ) -> Result<(), AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for core subagent workflow tool admission: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if status.admission_failures.len() > baseline_failures
                    && let Some(failure) = status.admission_failures.last()
                {
                    return Err(map_admission_failure_to_api_error(failure));
                }
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            if has_all_core_subagent_bindings(&loaded.state) {
                return Ok(());
            }
            tokio::time::sleep(self.poll_interval).await;
        }
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
                if status.admission_failures.len() > baseline_failures
                    && let Some(failure) = status.admission_failures.last()
                {
                    return Err(map_admission_failure_to_api_error(failure));
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
        let loaded =
            crate::checkpoint::load_reduction(self.store.as_ref(), self.store.as_ref(), &record)
                .await
                .map_err(|error| AgentApiError::internal(error.to_string()))?;
        if crate::checkpoint::checkpoint_due(&loaded)
            && let Err(error) = crate::checkpoint::write_checkpoint(
                self.store.as_ref(),
                self.store.as_ref(),
                &record,
                &loaded.reduced,
                record.updated_at_ms,
            )
            .await
        {
            tracing::warn!(session_id = %session_id, error = %error, "session checkpoint write failed after gateway replay");
        }
        Ok(LoadedSession {
            record,
            state: loaded.reduced.core_state,
        })
    }

    async fn load_retention_root(
        &self,
        record: &engine::storage::SessionRecord,
    ) -> Result<engine::storage::SessionRecord, AgentApiError> {
        if record.retention_root_session_id == record.session_id {
            return Ok(record.clone());
        }
        self.store
            .load_session(&record.retention_root_session_id)
            .await
            .map_err(map_session_store_error)?
            .ok_or_else(|| {
                AgentApiError::internal(format!(
                    "retention root {} is missing for session {}",
                    record.retention_root_session_id, record.session_id
                ))
            })
    }

    async fn project_session_by_id(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionView, AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        let retention_root = self.load_retention_root(&loaded.record).await?;
        let retention = session_retention_view(&loaded.record, &retention_root);
        self.projector()
            .project_session(ProjectSession {
                session_id,
                state: &loaded.state,
                record: &loaded.record,
                retention: &retention,
                run_limit: DEFAULT_RUN_SUMMARY_LIMIT,
                run_cursor: None,
            })
            .await
            .map(|(session, _, _)| session)
    }

    async fn project_session_page_by_id(
        &self,
        session_id: &SessionId,
        run_limit: usize,
    ) -> Result<(SessionView, Option<api::RunId>, bool), AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        let retention_root = self.load_retention_root(&loaded.record).await?;
        let retention = session_retention_view(&loaded.record, &retention_root);
        self.projector()
            .project_session(ProjectSession {
                session_id,
                state: &loaded.state,
                record: &loaded.record,
                retention: &retention,
                run_limit,
                run_cursor: None,
            })
            .await
    }

    fn session_mutation_view(&self, loaded: &LoadedSession) -> SessionMutationView {
        SessionMutationView {
            id: loaded.record.session_id.as_str().to_owned(),
            status: api_projection::session_status(&loaded.state),
            head_cursor: loaded
                .record
                .head
                .as_ref()
                .map(|head| event_cursor(head.seq)),
            config_revision: loaded.state.lifecycle.config_revision,
            context_revision: loaded.state.context.revision,
        }
    }

    async fn session_mutation_view_by_id(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionMutationView, AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        Ok(self.session_mutation_view(&loaded))
    }

    async fn project_run_by_id(
        &self,
        session_id: &SessionId,
        run_id: RunId,
    ) -> Result<RunView, AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        self.project_loaded_run(&loaded, run_id).await
    }

    /// Project one run from its complete sequence interval. Run projection is
    /// stateful across events (tool batches, approval pairs), so a partial
    /// interval would silently drop cross-event state; the interval is always
    /// read to completion and one pathological run is rejected with a typed
    /// error instead of a truncated view.
    async fn project_loaded_run(
        &self,
        loaded: &LoadedSession,
        run_id: RunId,
    ) -> Result<RunView, AgentApiError> {
        let metadata = run_projection_metadata(&loaded.state, run_id)
            .ok_or_else(|| AgentApiError::not_found(format!("run not found: {run_id}")))?;
        let entries = self
            .read_run_interval(
                &loaded.record,
                run_id,
                metadata.first_seq,
                metadata.terminal_seq,
            )
            .await?;
        self.projector()
            .project_run_with_metadata(ProjectRun {
                entries: &entries,
                run_id,
                status: metadata.status,
                output: metadata.output,
                source: metadata.source,
                started_at_ms: metadata.started_at_ms,
                completed_at_ms: metadata.completed_at_ms,
                usage: metadata.usage,
            })
            .await
    }

    async fn read_run_interval(
        &self,
        record: &engine::storage::SessionRecord,
        run_id: RunId,
        first_seq: engine::EventSeq,
        terminal_seq: Option<engine::EventSeq>,
    ) -> Result<Vec<engine::CoreAgentEntry>, AgentApiError> {
        let through = terminal_seq
            .or_else(|| record.head.as_ref().map(|head| head.seq))
            .unwrap_or(first_seq);
        let mut after = engine::EventSeq::new(first_seq.as_u64().saturating_sub(1));
        let codec = engine::CoreAgentCodec;
        let mut entries = Vec::new();
        let mut scanned: usize = 0;
        loop {
            let page = self
                .store
                .read_range(engine::storage::ReadSessionEventRange {
                    session_id: record.session_id.clone(),
                    after,
                    through,
                    limit: MAX_RUN_DETAIL_LIMIT,
                })
                .await
                .map_err(map_session_store_error)?;
            scanned = scanned.saturating_add(page.entries.len());
            if scanned > MAX_RUN_DETAIL_EVENTS {
                return Err(AgentApiError::rejected(format!(
                    "run {run_id} spans more than {MAX_RUN_DETAIL_EVENTS} events; page it through session/events/read"
                )));
            }
            for entry in &page.entries {
                let belongs = entry
                    .joins
                    .get("run_id")
                    .is_some_and(|value| value == &run_id.as_u64().to_string());
                if belongs {
                    entries.push(decode_stored_entry(&codec, entry)?);
                }
            }
            let Some(next_after) = page.next_after else {
                break;
            };
            if page.complete {
                break;
            }
            after = next_after;
        }
        Ok(entries)
    }
}

pub(super) struct LoadedSession {
    pub(super) record: engine::storage::SessionRecord,
    pub(super) state: engine::CoreAgentState,
}

struct RunProjectionMetadata<'a> {
    output: Option<&'a engine::ContentRef>,
    status: api::RunStatus,
    first_seq: engine::EventSeq,
    terminal_seq: Option<engine::EventSeq>,
    source: &'a engine::RunSource,
    started_at_ms: Option<u64>,
    completed_at_ms: Option<u64>,
    usage: Option<&'a engine::LlmUsage>,
}

fn run_projection_metadata(
    state: &engine::CoreAgentState,
    run_id: RunId,
) -> Option<RunProjectionMetadata<'_>> {
    state
        .runs
        .completed
        .iter()
        .find(|run| run.run_id == run_id)
        .map(|run| RunProjectionMetadata {
            status: core_run_status_to_api_status(run.status),
            first_seq: run.first_seq,
            terminal_seq: Some(run.terminal_seq),
            output: run.output.as_ref(),
            source: &run.source,
            started_at_ms: run.started_at_ms,
            completed_at_ms: Some(run.completed_at_ms),
            usage: run.usage.as_ref(),
        })
        .or_else(|| {
            state
                .runs
                .active
                .as_ref()
                .filter(|run| run.run_id == run_id)
                .map(|run| RunProjectionMetadata {
                    status: core_run_status_to_api_status(run.status),
                    first_seq: run.first_seq,
                    terminal_seq: None,
                    output: None,
                    source: &run.source,
                    started_at_ms: run.started_at_ms,
                    completed_at_ms: None,
                    usage: run.usage.as_ref(),
                })
        })
        .or_else(|| {
            state
                .runs
                .queued
                .iter()
                .find(|run| run.run_id == run_id)
                .map(|run| RunProjectionMetadata {
                    status: api::RunStatus::Queued,
                    first_seq: run.first_seq,
                    terminal_seq: None,
                    output: None,
                    source: &run.source,
                    started_at_ms: None,
                    completed_at_ms: None,
                    usage: None,
                })
        })
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
                        WorkflowToolCompletionKeySourceInput::ArrayItemField { pointer, field } => {
                            WorkflowToolCompletionKeySource::ArrayItemField { pointer, field }
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

fn is_core_environment_job_tool_id(tool_id: &str) -> bool {
    matches!(
        tool_id,
        JOB_SUBMIT_WORKFLOW_TOOL_ID | JOB_RUN_WORKFLOW_TOOL_ID
    )
}

fn is_core_environment_job_binding(binding: &engine::WorkflowToolBinding) -> bool {
    is_core_environment_job_tool_id(binding.definition.tool_id.as_str())
}

fn is_core_subagent_binding(binding: &engine::WorkflowToolBinding) -> bool {
    tools::subagents::is_subagent_workflow_tool_id(binding.definition.tool_id.as_str())
}

fn has_all_core_subagent_bindings(state: &engine::CoreAgentState) -> bool {
    [
        tools::subagents::AGENT_RUN_WORKFLOW_TOOL_ID,
        tools::subagents::AGENT_SPAWN_WORKFLOW_TOOL_ID,
    ]
    .into_iter()
    .all(|tool_id| {
        state
            .workflow_tools
            .bindings
            .contains_key(&WorkflowToolId::new(tool_id))
    })
}

fn validate_subagent_deadline_for_existing_bindings(
    state: &engine::CoreAgentState,
    features: &engine::FeaturesConfig,
) -> Result<(), AgentApiError> {
    let Some(requested_deadline_ms) = features
        .subagents
        .as_ref()
        .map(|subagents| subagents.limits.deadline_ms)
    else {
        return Ok(());
    };
    let existing_ceiling_ms = [
        tools::subagents::AGENT_RUN_WORKFLOW_TOOL_ID,
        tools::subagents::AGENT_SPAWN_WORKFLOW_TOOL_ID,
    ]
    .into_iter()
    .filter_map(|tool_id| {
        state
            .workflow_tools
            .bindings
            .get(&WorkflowToolId::new(tool_id))
    })
    .filter_map(|binding| match binding.completion {
        WorkflowToolCompletion::Joined {
            deadline_after_ms, ..
        } => Some(deadline_after_ms),
        WorkflowToolCompletion::Promises {
            deadline_after_ms, ..
        } => deadline_after_ms,
        WorkflowToolCompletion::Accepted => None,
    })
    .min();
    if let Some(ceiling_ms) = existing_ceiling_ms
        && requested_deadline_ms > ceiling_ms
    {
        return Err(AgentApiError::invalid_request(format!(
            "subagents deadlineMs {requested_deadline_ms} exceeds this session's immutable binding ceiling of {ceiling_ms} ms; create a new session to use the 24-hour ceiling"
        )));
    }
    Ok(())
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
    // ── Bots ────────────────────────────────────────────────────────────

    async fn create_bot(
        &self,
        params: BotCreateParams,
    ) -> Result<AgentApiOutcome<BotCreateResponse>, AgentApiError> {
        self.create_bot_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn put_bot(
        &self,
        params: BotPutParams,
    ) -> Result<AgentApiOutcome<BotPutResponse>, AgentApiError> {
        let bot = self
            .put_bot_record(params.bot, params.expected_revision)
            .await?;
        Ok(AgentApiOutcome::new(BotPutResponse { bot: bot.view() }))
    }

    async fn read_bot(
        &self,
        params: BotReadParams,
    ) -> Result<AgentApiOutcome<BotReadResponse>, AgentApiError> {
        let bot = ::bots::BotStore::read_bot(self.store.as_ref(), &params.bot_id)
            .await
            .map_err(crate::bots::map_bot_error)?;
        Ok(AgentApiOutcome::new(BotReadResponse { bot: bot.view() }))
    }

    async fn list_bots(
        &self,
        _params: BotListParams,
    ) -> Result<AgentApiOutcome<BotListResponse>, AgentApiError> {
        self.list_bot_roster().await.map(AgentApiOutcome::new)
    }

    async fn close_bot(
        &self,
        params: BotCloseParams,
    ) -> Result<AgentApiOutcome<BotCloseResponse>, AgentApiError> {
        let bot = self.close_bot_record(&params.bot_id).await?;
        Ok(AgentApiOutcome::new(BotCloseResponse { bot: bot.view() }))
    }

    async fn delete_bot(
        &self,
        params: BotDeleteParams,
    ) -> Result<AgentApiOutcome<BotDeleteResponse>, AgentApiError> {
        let (bot, deleted_sessions) = self.delete_bot_record(&params.bot_id).await?;
        Ok(AgentApiOutcome::new(BotDeleteResponse {
            bot: bot.view(),
            deleted_sessions,
        }))
    }

    async fn read_bot_state(
        &self,
        params: BotStateReadParams,
    ) -> Result<AgentApiOutcome<BotStateReadResponse>, AgentApiError> {
        let state = self.bot_state_view(&params.bot_id).await?;
        Ok(AgentApiOutcome::new(BotStateReadResponse { state }))
    }

    async fn rotate_bot_session(
        &self,
        params: BotSessionRotateParams,
    ) -> Result<AgentApiOutcome<BotSessionRotateResponse>, AgentApiError> {
        let accepted = self
            .rotate_bot_session_record(&params.bot_id, &params.session_id)
            .await?;
        Ok(AgentApiOutcome::new(BotSessionRotateResponse { accepted }))
    }

    async fn put_bot_trigger(
        &self,
        params: BotTriggerPutParams,
    ) -> Result<AgentApiOutcome<BotTriggerPutResponse>, AgentApiError> {
        let record = self
            .put_bot_trigger_record(&params.bot_id, params.trigger, params.expected_revision)
            .await?;
        Ok(AgentApiOutcome::new(BotTriggerPutResponse {
            trigger: self.trigger_view(&record),
        }))
    }

    async fn read_bot_trigger(
        &self,
        params: BotTriggerReadParams,
    ) -> Result<AgentApiOutcome<BotTriggerReadResponse>, AgentApiError> {
        let record = ::bots::BotTriggerStore::read_bot_trigger(
            self.store.as_ref(),
            &params.bot_id,
            &params.trigger_id,
        )
        .await
        .map_err(crate::bots::map_bot_error)?;
        Ok(AgentApiOutcome::new(BotTriggerReadResponse {
            trigger: self.trigger_view(&record),
        }))
    }

    async fn list_bot_triggers(
        &self,
        params: BotTriggerListParams,
    ) -> Result<AgentApiOutcome<BotTriggerListResponse>, AgentApiError> {
        ::bots::BotStore::read_bot(self.store.as_ref(), &params.bot_id)
            .await
            .map_err(crate::bots::map_bot_error)?;
        let records =
            ::bots::BotTriggerStore::list_bot_triggers(self.store.as_ref(), &params.bot_id)
                .await
                .map_err(crate::bots::map_bot_error)?;
        Ok(AgentApiOutcome::new(BotTriggerListResponse {
            triggers: records
                .iter()
                .map(|record| self.trigger_view(record))
                .collect(),
        }))
    }

    async fn delete_bot_trigger(
        &self,
        params: BotTriggerDeleteParams,
    ) -> Result<AgentApiOutcome<BotTriggerDeleteResponse>, AgentApiError> {
        let record = self
            .delete_bot_trigger_record(&params.bot_id, &params.trigger_id)
            .await?;
        Ok(AgentApiOutcome::new(BotTriggerDeleteResponse {
            trigger: self.trigger_view(&record),
        }))
    }

    async fn admit_bot_event(
        &self,
        params: BotEventAdmitParams,
    ) -> Result<AgentApiOutcome<BotEventAdmitResponse>, AgentApiError> {
        let (record, duplicate) = self
            .admit_bot_event_record(&params.bot_id, params.event)
            .await?;
        Ok(AgentApiOutcome::new(BotEventAdmitResponse {
            event: record.view(),
            duplicate,
        }))
    }

    async fn replay_bot_event(
        &self,
        params: BotEventReplayParams,
    ) -> Result<AgentApiOutcome<BotEventReplayResponse>, AgentApiError> {
        let record = self
            .replay_bot_event_record(&params.bot_id, params.seq)
            .await?;
        Ok(AgentApiOutcome::new(BotEventReplayResponse {
            event: record.view(),
        }))
    }

    async fn list_bot_events(
        &self,
        params: BotEventListParams,
    ) -> Result<AgentApiOutcome<BotEventListResponse>, AgentApiError> {
        let (records, next_cursor) = self
            .list_bot_events_page(&params.bot_id, params.limit, params.cursor)
            .await?;
        Ok(AgentApiOutcome::new(BotEventListResponse {
            events: records.iter().map(|record| record.view()).collect(),
            next_cursor,
        }))
    }

    async fn read_bot_event(
        &self,
        params: BotEventReadParams,
    ) -> Result<AgentApiOutcome<BotEventReadResponse>, AgentApiError> {
        self.read_bot_event_with_document(&params.bot_id, params.seq)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn test_bot_filter(
        &self,
        params: BotFilterTestParams,
    ) -> Result<AgentApiOutcome<BotFilterTestResponse>, AgentApiError> {
        self.test_bot_filter_records(params)
            .await
            .map(AgentApiOutcome::new)
    }

    // ── Channels ────────────────────────────────────────────────────────

    async fn create_channel_account(
        &self,
        params: ChannelAccountCreateParams,
    ) -> Result<AgentApiOutcome<ChannelAccountCreateResponse>, AgentApiError> {
        self.create_channel_account_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn put_channel_account(
        &self,
        params: ChannelAccountPutParams,
    ) -> Result<AgentApiOutcome<ChannelAccountPutResponse>, AgentApiError> {
        self.put_channel_account_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn read_channel_account(
        &self,
        params: ChannelAccountReadParams,
    ) -> Result<AgentApiOutcome<ChannelAccountReadResponse>, AgentApiError> {
        self.read_channel_account_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn list_channel_accounts(
        &self,
        params: ChannelAccountListParams,
    ) -> Result<AgentApiOutcome<ChannelAccountListResponse>, AgentApiError> {
        self.list_channel_account_records(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn delete_channel_account(
        &self,
        params: ChannelAccountDeleteParams,
    ) -> Result<AgentApiOutcome<ChannelAccountDeleteResponse>, AgentApiError> {
        self.delete_channel_account_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn admit_channel_inbound(
        &self,
        params: ChannelInboundAdmitParams,
    ) -> Result<AgentApiOutcome<ChannelInboundAdmitResponse>, AgentApiError> {
        self.admit_channel_inbound_message(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn list_channel_pairings(
        &self,
        params: ChannelPairingListParams,
    ) -> Result<AgentApiOutcome<ChannelPairingListResponse>, AgentApiError> {
        self.list_channel_pairing_records(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn delete_channel_pairing(
        &self,
        params: ChannelPairingDeleteParams,
    ) -> Result<AgentApiOutcome<ChannelPairingDeleteResponse>, AgentApiError> {
        self.delete_channel_pairing_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn read_channel_conversation(
        &self,
        params: ChannelConversationReadParams,
    ) -> Result<AgentApiOutcome<ChannelConversationReadResponse>, AgentApiError> {
        self.read_channel_conversation_snapshot(params)
            .await
            .map(AgentApiOutcome::new)
    }

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
                version: format!("{}+{}", release_info::VERSION, release_info::GIT_SHA),
                git_sha: release_info::GIT_SHA.to_owned(),
                envd: EnvironmentDaemonInfo {
                    version: release_info::VERSION.to_owned(),
                    git_sha: release_info::GIT_SHA.to_owned(),
                    protocol_version: environment_protocol::shared::CURRENT_PROTOCOL_VERSION,
                    targets: release_info::envd_targets().map(str::to_owned).collect(),
                },
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
    /// exists, the existing session view is returned (creation fields such as
    /// config, metadata, profile, and environment override are ignored).
    /// This keeps a retried `session/start` + `session/runs/start` pair safe
    /// end to end.
    async fn start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        self.start_session_internal(params, false, false, None)
            .await
    }

    async fn start_managed_session(
        &self,
        params: ManagedSessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        let ManagedSessionStartParams {
            session_id,
            display_name,
            metadata,
            config,
            profile,
            environment,
            delete_after_close_ms,
            workflow_tools,
        } = params;
        let workflow_tools = managed_workflow_tools_from_api(workflow_tools)?;
        self.start_session_internal(
            SessionStartParams {
                session_id,
                display_name,
                metadata,
                config,
                profile,
                environment,
                delete_after_close_ms,
            },
            false,
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
        self.validate_subagent_agents(&config.features).await?;
        validate_subagent_deadline_for_existing_bindings(&loaded.state, &config.features)?;
        if &config == current_config {
            // The config event is an idempotent no-op, but derived tools and
            // managed context may still need repair or reflect newer
            // universe-owned registry records.
            let _ = self
                .configure_session_toolset(&session_id, &loaded, true)
                .await?;
            self.load_session_state_with_current_run_context(&session_id)
                .await?;
            return Ok(AgentApiOutcome::new(SessionConfigPutResponse {
                session: self.session_mutation_view_by_id(&session_id).await?,
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
        let _ = self
            .configure_session_toolset(&session_id, &loaded, true)
            .await?;
        self.load_session_state_with_current_run_context(&session_id)
            .await?;
        let session = self.session_mutation_view_by_id(&session_id).await?;
        Ok(AgentApiOutcome::new(SessionConfigPutResponse { session }))
    }

    async fn read_session(
        &self,
        params: SessionReadParams,
    ) -> Result<AgentApiOutcome<SessionReadResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        if let Some(status) = self.query_status_optional(&session_id).await?
            && let Some(error) = status.last_error
        {
            return Err(AgentApiError::internal(format!(
                "agent workflow reported error: {error}"
            )));
        }
        let limit = match params.run_limit {
            Some(0) => return Err(AgentApiError::invalid_request("runLimit must be positive")),
            Some(limit) => (limit as usize).min(MAX_RUN_SUMMARY_LIMIT),
            None => DEFAULT_RUN_SUMMARY_LIMIT,
        };
        let (session, next_run_cursor, has_older_runs) =
            self.project_session_page_by_id(&session_id, limit).await?;
        tracing::debug!(
            session_id = %session_id,
            summary_page_size = session.runs.len(),
            preview_blob_reads_upper_bound = session.runs.len(),
            has_older_runs,
            "projected bounded session summary"
        );
        Ok(AgentApiOutcome::new(SessionReadResponse {
            session,
            next_run_cursor,
            has_older_runs,
        }))
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
        let root_session_id = params
            .root_session_id
            .map(SessionId::try_new)
            .transpose()
            .map_err(|error| {
                AgentApiError::invalid_request(format!("invalid rootSessionId: {error}"))
            })?;
        let parent_session_id = params
            .parent_session_id
            .map(SessionId::try_new)
            .transpose()
            .map_err(|error| {
                AgentApiError::invalid_request(format!("invalid parentSessionId: {error}"))
            })?;
        let page = self
            .store
            .list_sessions(engine::storage::ListSessions {
                cursor,
                limit,
                root_session_id,
                parent_session_id,
                exclude_closed: params.exclude_closed,
                metadata: params.metadata,
            })
            .await
            .map_err(map_session_store_error)?;
        let mut sessions = Vec::with_capacity(page.sessions.len());
        for record in page.sessions {
            let root = self.load_retention_root(&record).await?;
            sessions.push(session_summary_view(record, &root));
        }
        Ok(AgentApiOutcome::new(SessionListResponse {
            sessions,
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
        let root = self.load_retention_root(&record).await?;
        Ok(AgentApiOutcome::new(SessionRenameResponse {
            session: session_summary_view(record, &root),
        }))
    }

    async fn put_session_metadata(
        &self,
        params: SessionMetadataPutParams,
    ) -> Result<AgentApiOutcome<SessionMetadataPutResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        validate_caller_metadata(&params.metadata)?;
        let record = self
            .store
            .set_session_metadata(&session_id, params.metadata)
            .await
            .map_err(map_session_store_error)?;
        let root = self.load_retention_root(&record).await?;
        Ok(AgentApiOutcome::new(SessionMetadataPutResponse {
            session: session_summary_view(record, &root),
        }))
    }

    async fn put_session_retention(
        &self,
        params: SessionRetentionPutParams,
    ) -> Result<AgentApiOutcome<SessionRetentionPutResponse>, AgentApiError> {
        validate_delete_after_close_ms(params.delete_after_close_ms)?;
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let record = self
            .store
            .set_session_retention(&session_id, params.delete_after_close_ms)
            .await
            .map_err(map_session_store_error)?;
        let root = record.clone();
        Ok(AgentApiOutcome::new(SessionRetentionPutResponse {
            session: session_summary_view(record, &root),
        }))
    }

    async fn read_session_events(
        &self,
        params: SessionEventsReadParams,
    ) -> Result<AgentApiOutcome<SessionEventsReadResponse>, AgentApiError> {
        if params.direction == SessionEventDirection::Backward {
            return event_history::read(self.store.as_ref(), self.store.as_ref(), params)
                .await
                .map(AgentApiOutcome::new);
        }
        if params.before.is_some() {
            return Err(AgentApiError::invalid_request(
                "before requires backward direction",
            ));
        }
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
                session: self.session_mutation_view_by_id(&session_id).await?,
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
            self.wait_for_closed_session(&session_id).await?;
            let session = self.session_mutation_view_by_id(&session_id).await?;
            self.close_session_owned_environments(&session_id).await;
            return Ok(AgentApiOutcome::new(SessionCloseResponse { session }));
        }

        // Force path. Prefer the live workflow: it cancels active work,
        // appends the close, observes closed+quiescent, and exits itself.
        if self.workflow_is_running(&session_id).await {
            let signalled = self
                .submit_core_command(&session_id, CoreAgentCommand::CloseSession { force: true })
                .await
                .is_ok();
            if signalled && self.wait_for_closed_session(&session_id).await.is_ok() {
                self.close_session_owned_environments(&session_id).await;
                let session = self.session_mutation_view_by_id(&session_id).await?;
                return Ok(AgentApiOutcome::new(SessionCloseResponse { session }));
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
        self.close_session_owned_environments(&session_id).await;
        let session = self.session_mutation_view_by_id(&session_id).await?;
        Ok(AgentApiOutcome::new(SessionCloseResponse { session }))
    }

    async fn delete_session(
        &self,
        params: SessionDeleteParams,
    ) -> Result<AgentApiOutcome<SessionDeleteResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let target_before_delete = self
            .store
            .load_session(&session_id)
            .await
            .map_err(map_session_store_error)?
            .ok_or_else(|| AgentApiError::not_found(format!("session not found: {session_id}")))?;
        let root = self.load_retention_root(&target_before_delete).await?;
        let deleted = crate::session_deletion::delete_session_subtree(
            self.store.as_ref(),
            engine::storage::DeleteClosedSessions {
                session_id: session_id.clone(),
                cascade: params.cascade,
                due_at_or_before_ms: None,
            },
            crate::session_deletion::SessionDeletionCause::Manual,
        )
        .await
        .map_err(map_session_store_error)?;
        Ok(AgentApiOutcome::new(SessionDeleteResponse {
            session: session_summary_view(deleted.target, &root),
            deleted_session_count: deleted.deleted_session_ids.len() as u64,
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
        self.wait_for_context_compaction_complete(
            &session_id,
            baseline_revision,
            baseline_failures,
        )
        .await?;
        let session = self.session_mutation_view_by_id(&session_id).await?;
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
            let key = parse_client_context_key(entry.key.clone())?;
            if !seen_keys.insert(key.clone()) {
                return Err(AgentApiError::invalid_request(format!(
                    "duplicate context key in append batch: {key}"
                )));
            }
            match context_entry_input_from_api(self.store.as_ref(), &entry.item).await {
                Ok(input) => {
                    let text = match &entry.item {
                        InputItem::Text { text, .. } => Some(text.trim().to_owned()),
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
                    if let Some(active) = engine::current_context_entry(&loaded.state, &key)
                        .filter(|active| active_context_entry_matches_input(active, &input))
                    {
                        let effective = active_entry_input(active);
                        let text = text
                            .filter(|_| effective.content.content_ref == input.content.content_ref);
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
                                .filter(|_| entry.content.content_ref == input.content.content_ref);
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
            let key = parse_client_context_key(key)?;
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

    async fn list_runs(
        &self,
        params: RunListParams,
    ) -> Result<AgentApiOutcome<RunListResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let limit = match params.limit {
            Some(0) => return Err(AgentApiError::invalid_request("limit must be positive")),
            Some(limit) => (limit as usize).min(MAX_RUN_SUMMARY_LIMIT),
            None => DEFAULT_RUN_SUMMARY_LIMIT,
        };
        let cursor = params.cursor.as_deref().map(parse_api_run_id).transpose()?;
        let loaded = self.load_session_state(&session_id).await?;
        let (runs, next_cursor, has_older_runs) = self
            .projector()
            .project_run_summaries(&loaded.state, cursor, limit)
            .await?;
        tracing::debug!(
            session_id = %session_id,
            summary_page_size = runs.len(),
            preview_blob_reads_upper_bound = runs.len(),
            has_older_runs,
            "projected run summary page"
        );
        Ok(AgentApiOutcome::new(RunListResponse {
            runs,
            next_cursor,
            has_older_runs,
        }))
    }

    async fn read_run(
        &self,
        params: RunReadParams,
    ) -> Result<AgentApiOutcome<RunReadResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let run_id = parse_api_run_id(&params.run_id)?;
        let loaded = self.load_session_state(&session_id).await?;
        let run = self.project_loaded_run(&loaded, run_id).await?;
        tracing::debug!(
            session_id = %session_id,
            run_id = run_id.as_u64(),
            detail_entries = run.entries.len(),
            "projected complete run detail"
        );
        Ok(AgentApiOutcome::new(RunReadResponse { run }))
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
                        RunStatus::Active | RunStatus::Parked | RunStatus::Cancelling
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

    async fn decide_run_approvals(
        &self,
        params: RunApprovalsDecideParams,
    ) -> Result<AgentApiOutcome<RunApprovalsDecideResponse>, AgentApiError> {
        const MAX_DECISIONS: usize = 64;
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let run_id = parse_api_run_id(&params.run_id)?;
        if params.decisions.is_empty() || params.decisions.len() > MAX_DECISIONS {
            return Err(AgentApiError::invalid_request(format!(
                "session/runs/approvals/decide requires 1 to {MAX_DECISIONS} decisions"
            )));
        }
        let loaded = self.load_session_state(&session_id).await?;
        let Some(active) = loaded.state.runs.active.as_ref() else {
            return Err(AgentApiError::rejected(format!(
                "run is not active: {}",
                params.run_id
            )));
        };
        if active.run_id != run_id
            || !matches!(active.status, RunStatus::Active | RunStatus::Parked)
        {
            return Err(AgentApiError::rejected(format!(
                "run is not accepting approval decisions: {}",
                params.run_id
            )));
        }

        let principal = crate::gateway::principal::request_principal();
        let decided_by = engine::ApprovalPrincipal {
            kind: match principal.kind {
                auth::PrincipalKind::User => "user",
                auth::PrincipalKind::ServiceAccount => "service_account",
                auth::PrincipalKind::UniverseDefault => "universe_default",
            }
            .to_owned(),
            id: principal.id,
        };
        let mut seen = BTreeSet::new();
        let mut results = Vec::with_capacity(params.decisions.len());
        for input in params.decisions {
            let approval_id = match ApprovalId::try_new(input.approval_id.clone()) {
                Ok(id) => id,
                Err(error) => {
                    results.push(approval_decision_failure(
                        input.approval_id,
                        ApprovalDecisionFailureKind::InvalidId,
                        error.to_string(),
                    ));
                    continue;
                }
            };
            if !seen.insert(approval_id.clone()) {
                results.push(approval_decision_failure(
                    input.approval_id,
                    ApprovalDecisionFailureKind::Duplicate,
                    "duplicate approval id in decision batch",
                ));
                continue;
            }
            if let Err(error) = engine::validate_note(input.note.as_deref()) {
                results.push(approval_decision_failure(
                    input.approval_id,
                    ApprovalDecisionFailureKind::InvalidNote,
                    error.to_string(),
                ));
                continue;
            }
            let Some(record) = active.approvals.get(&approval_id) else {
                results.push(approval_decision_failure(
                    input.approval_id,
                    ApprovalDecisionFailureKind::Unknown,
                    "approval was not found for the active run",
                ));
                continue;
            };
            if record.request.run_id != run_id {
                results.push(approval_decision_failure(
                    input.approval_id,
                    ApprovalDecisionFailureKind::ForeignRun,
                    "approval belongs to a different run",
                ));
                continue;
            }
            if record.status != engine::ApprovalStatus::Pending {
                let kind = if record.status == engine::ApprovalStatus::Cancelled {
                    ApprovalDecisionFailureKind::Cancelled
                } else {
                    ApprovalDecisionFailureKind::AlreadyDecided
                };
                results.push(approval_decision_failure(
                    input.approval_id,
                    kind,
                    "approval is already terminal",
                ));
                continue;
            }
            let engine_decision = match input.decision {
                ApprovalDecisionKind::Approve => engine::ApprovalDecision::Approved,
                ApprovalDecisionKind::Reject => engine::ApprovalDecision::Rejected,
            };
            let approve = engine_decision == engine::ApprovalDecision::Approved;
            let response = match &record.request.continuation {
                engine::ApprovalContinuation::OpenAiMcp {
                    provider_request_id,
                } => {
                    let response_json = serde_json::json!({
                        "type": "mcp_approval_response",
                        "approval_request_id": provider_request_id,
                        "approve": approve,
                    });
                    let response_ref = self
                        .store
                        .put_bytes(serde_json::to_vec(&response_json).map_err(|error| {
                            AgentApiError::internal(format!(
                                "encode MCP approval response: {error}"
                            ))
                        })?)
                        .await
                        .map_err(|error| AgentApiError::internal(error.to_string()))?;
                    Some(ContextEntryInput {
                        kind: ContextEntryKind::McpApprovalResponse {
                            approval_request_id: provider_request_id.clone(),
                            approve,
                        },
                        content: engine::ContentRef {
                            content_ref: response_ref,
                            media_type: Some("application/json".to_owned()),
                            provider_kind: Some(
                                "openai.responses.mcp_approval_response".to_owned(),
                            ),
                        },
                        preview: Some(
                            if approve {
                                "MCP tool call approved"
                            } else {
                                "MCP tool call rejected"
                            }
                            .to_owned(),
                        ),
                        origin: None,
                        provenance_ref: None,
                        token_estimate: None,
                    })
                }
                engine::ApprovalContinuation::NativeMcp { .. } => None,
            };
            let correlation = format!("approval_{}", uuid::Uuid::new_v4().simple());
            self.signal_submit_admissions(
                &session_id,
                vec![AgentAdmission {
                    command: CoreAgentCommand::DecideApproval(engine::ApprovalDecisionCommand {
                        approval_id: approval_id.clone(),
                        run_id,
                        decision: engine_decision,
                        note: input.note,
                        decided_by: Some(decided_by.clone()),
                        response,
                    }),
                    correlation_token: Some(correlation.clone()),
                }],
            )
            .await?;
            match self
                .wait_for_approval_decision(&session_id, run_id, &approval_id, &correlation)
                .await
            {
                Ok(()) => results.push(ApprovalDecisionResult {
                    approval_id: approval_id.as_str().to_owned(),
                    status: ApprovalDecisionStatus::Decided,
                    failure: None,
                }),
                Err(error) => results.push(approval_decision_failure(
                    approval_id.as_str().to_owned(),
                    ApprovalDecisionFailureKind::Rejected,
                    error.message,
                )),
            }
        }
        let run = self.project_run_by_id(&session_id, run_id).await?;
        Ok(AgentApiOutcome::new(RunApprovalsDecideResponse {
            results,
            run,
        }))
    }

    /// Steer the active run. The steering is admitted against the
    /// live drive — between drive actions or while a model/tool activity is
    /// in flight — and materializes at the run's next turn boundary. A
    /// parked run accepts steering without waking.
    async fn steer_run(
        &self,
        params: RunSteerParams,
    ) -> Result<AgentApiOutcome<RunSteerResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let requested_run_id = parse_api_run_id(&params.run_id)?;
        if params.items.is_empty() {
            return Err(AgentApiError::invalid_request(
                "session/runs/steer requires at least one input item",
            ));
        }
        let loaded = self.load_session_state(&session_id).await?;
        if loaded.state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        let steering_baseline = match loaded.state.runs.active.as_ref() {
            Some(active)
                if active.run_id == requested_run_id
                    && matches!(active.status, RunStatus::Active | RunStatus::Parked) =>
            {
                active.steering.len()
            }
            Some(active) if active.run_id == requested_run_id => {
                return Err(AgentApiError::rejected(format!(
                    "run is not accepting steering: {}",
                    params.run_id
                )));
            }
            _ if loaded
                .state
                .runs
                .queued
                .iter()
                .any(|run| run.run_id == requested_run_id) =>
            {
                return Err(AgentApiError::rejected(format!(
                    "run is queued and cannot be steered yet: {}",
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
        };
        let input = run_input_from_api(self.store.as_ref(), &params.items).await?;
        let correlation_token = format!("steer_{}", uuid::Uuid::new_v4().simple());
        self.signal_submit_admissions(
            &session_id,
            vec![AgentAdmission {
                command: CoreAgentCommand::RequestRunSteering { input },
                correlation_token: Some(correlation_token.clone()),
            }],
        )
        .await?;
        let steering_id = self
            .wait_for_steering_accepted(
                &session_id,
                requested_run_id,
                steering_baseline,
                &correlation_token,
            )
            .await?;
        let run = self
            .project_run_by_id(&session_id, requested_run_id)
            .await?;
        Ok(AgentApiOutcome::new(RunSteerResponse {
            steering_id: api_steering_id(steering_id),
            run,
        }))
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

    async fn create_external_environment(
        &self,
        params: EnvironmentExternalCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentExternalCreateResponse>, AgentApiError> {
        self.create_external_environment_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn create_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentRegistrationKeyCreateResponse>, AgentApiError> {
        self.create_environment_registration_key_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn read_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentRegistrationKeyReadResponse>, AgentApiError> {
        self.read_environment_registration_key_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn list_environment_registration_keys(
        &self,
        params: EnvironmentRegistrationKeyListParams,
    ) -> Result<AgentApiOutcome<EnvironmentRegistrationKeyListResponse>, AgentApiError> {
        self.list_environment_registration_key_records(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn revoke_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyRevokeParams,
    ) -> Result<AgentApiOutcome<EnvironmentRegistrationKeyRevokeResponse>, AgentApiError> {
        self.revoke_environment_registration_key_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn put_environment_ingress(
        &self,
        params: EnvironmentIngressPutParams,
    ) -> Result<AgentApiOutcome<EnvironmentIngressPutResponse>, AgentApiError> {
        self.put_environment_ingress_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn put_environment_power(
        &self,
        params: EnvironmentPowerPutParams,
    ) -> Result<AgentApiOutcome<EnvironmentPowerPutResponse>, AgentApiError> {
        self.put_environment_power_record(params)
            .await
            .map(AgentApiOutcome::new)
    }

    async fn put_environment_idle_policy(
        &self,
        params: EnvironmentIdlePolicyPutParams,
    ) -> Result<AgentApiOutcome<EnvironmentIdlePolicyPutResponse>, AgentApiError> {
        self.put_environment_idle_policy_record(params)
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
        let response =
            commit_vfs_snapshot(self.store.as_ref(), Some(self.store.as_ref()), params).await?;
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

    async fn discover_mcp_server_auth(
        &self,
        params: McpServerAuthDiscoverParams,
    ) -> Result<AgentApiOutcome<McpServerAuthDiscoverResponse>, AgentApiError> {
        mcp::validate_remote_mcp_server_url(&params.server_url).map_err(map_mcp_error)?;
        let target = auth::McpOAuthTarget {
            server_id: "discovery".to_owned(),
            server_url: params.server_url,
            scopes_default: Vec::new(),
            protected_resource_metadata_url: None,
            authorization_server_hint: None,
        };
        let discovered = tokio::time::timeout(
            Duration::from_secs(5),
            self.mcp_oauth.discover_protected_resource(&target),
        )
        .await;
        let oauth = match discovered {
            Ok(Ok(metadata)) => Some(McpOAuthDiscoveryView {
                resource: metadata.resource,
                authorization_servers: metadata.authorization_servers,
                scopes_supported: metadata.scopes_supported,
            }),
            Ok(Err(auth::McpOAuthError::ProtectedResourceMetadataUnavailable { .. })) | Err(_) => {
                None
            }
            Ok(Err(error)) => return Err(map_mcp_oauth_error(error)),
        };
        Ok(AgentApiOutcome::new(McpServerAuthDiscoverResponse {
            oauth,
        }))
    }

    async fn discover_mcp_server_tools(
        &self,
        params: McpServerToolsDiscoverParams,
    ) -> Result<AgentApiOutcome<McpServerToolsDiscoverResponse>, AgentApiError> {
        let server_id = parse_mcp_server_id(params.server_id)?;
        let record = self
            .store
            .read_server(&server_id)
            .await
            .map_err(map_mcp_error)?;
        if record.status == mcp::McpServerStatus::Disabled {
            return Err(AgentApiError::rejected(format!(
                "MCP server is disabled: {server_id}"
            )));
        }
        if record.status == mcp::McpServerStatus::NeedsAuthConfig
            || (record.auth_grant_id.is_none()
                && matches!(
                    record.auth_policy,
                    mcp::McpServerAuthPolicy::RequiredBearer
                        | mcp::McpServerAuthPolicy::RequiredOAuth { .. }
                ))
        {
            return Ok(AgentApiOutcome::new(mcp_api::mcp_tool_discovery_failure(
                mcp::McpToolDiscoveryFailure::new(
                    mcp::McpToolDiscoveryFailureKind::CredentialAbsent,
                    "This MCP server needs a credential before its tools can be discovered",
                ),
            )));
        }

        let _discovery_permit = self
            .mcp_discovery_gate
            .try_start(server_id.as_str())
            .map_err(AgentApiError::rejected)?;
        let discovery_started = Instant::now();

        let trusted_universe = self
            .configurator_trusted_header
            .permits(server_id.as_str(), &record.server_url)
            .then_some(self.store.config().universe_id);
        let bearer = match (trusted_universe, record.auth_grant_id.as_ref()) {
            (Some(_), _) => None,
            (None, Some(grant_id)) => match self
                .auth_token_broker
                .bearer_token(
                    grant_id,
                    &TokenAudience::McpResource(record.server_url.clone()),
                )
                .await
            {
                Ok(token) => Some(token),
                Err(auth::AuthBrokerError::AudienceMismatch { .. }) => {
                    return Ok(AgentApiOutcome::new(mcp_api::mcp_tool_discovery_failure(
                        mcp::McpToolDiscoveryFailure::new(
                            mcp::McpToolDiscoveryFailureKind::GrantAudienceMismatch,
                            "The configured credential does not cover this MCP server",
                        ),
                    )));
                }
                Err(auth::AuthBrokerError::Store { message }) => {
                    return Err(AgentApiError::internal(message));
                }
                Err(_) => {
                    return Ok(AgentApiOutcome::new(mcp_api::mcp_tool_discovery_failure(
                        mcp::McpToolDiscoveryFailure::new(
                            mcp::McpToolDiscoveryFailureKind::GrantNeedsReauth,
                            "The configured credential must be reconnected",
                        ),
                    )));
                }
            },
            (None, None) => None,
        };
        let response = match self
            .mcp_tool_discoverer
            .discover_tools(
                &record.server_url,
                bearer.as_ref(),
                trusted_universe,
                self.mcp_private_networks
                    .permits(&record.server_url, record.allow_private_network),
                mcp::McpToolDiscoveryLimits::default(),
            )
            .await
        {
            Ok(inventory) => {
                tracing::info!(
                    server_id = %server_id,
                    outcome = "success",
                    tool_count = inventory.tools.len(),
                    duration_ms = discovery_started.elapsed().as_millis(),
                    "completed live MCP tool discovery"
                );
                mcp_api::mcp_tool_discovery_success(inventory.tools)
            }
            Err(failure) => {
                tracing::info!(
                    server_id = %server_id,
                    outcome = ?failure.kind,
                    tool_count = 0,
                    duration_ms = discovery_started.elapsed().as_millis(),
                    "completed live MCP tool discovery"
                );
                mcp_api::mcp_tool_discovery_failure(failure)
            }
        };
        Ok(AgentApiOutcome::new(response))
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

    async fn lease_auth_grant(
        &self,
        params: AuthGrantLeaseParams,
    ) -> Result<AgentApiOutcome<AuthGrantLeaseResponse>, AgentApiError> {
        let grant_id = parse_auth_grant_id(params.grant_id)?;
        let grant = self
            .store
            .read_grant(&grant_id)
            .await
            .map_err(map_auth_error)?;
        require_retrievable_grant(&grant)?;

        let audience = match grant.provider_kind {
            auth::AuthProviderKind::McpOAuth => {
                TokenAudience::McpResource(params.audience.ok_or_else(|| {
                    AgentApiError::rejected("mcp_oauth grant leases require an audience")
                })?)
            }
            auth::AuthProviderKind::GitHubApp => {
                TokenAudience::GitHubApi(params.audience.ok_or_else(|| {
                    AgentApiError::rejected("github_app grant leases require an audience")
                })?)
            }
            _ => TokenAudience::ServiceLease(
                params
                    .audience
                    .or_else(|| grant.audience.clone())
                    .unwrap_or_else(|| "service:lease".to_owned()),
            ),
        };
        let token = self
            .auth_token_broker
            .bearer_token(&grant_id, &audience)
            .await
            .map_err(map_auth_broker_error)?;
        let leased = self
            .store
            .record_grant_lease(&grant_id, now_ms()?)
            .await
            .map_err(map_auth_error)?;
        let principal = crate::gateway::principal::request_principal();
        tracing::info!(
            grant_id = %grant_id,
            principal_kind = ?principal.kind,
            principal_id = principal.id.as_deref().unwrap_or(""),
            "auth grant leased"
        );
        Ok(AgentApiOutcome::new(AuthGrantLeaseResponse {
            token: token.expose().to_owned(),
            expires_at_ms: leased.expires_at_ms,
            grant_id: grant_id.as_str().to_owned(),
            provider_kind: api_auth_provider_kind(leased.provider_kind),
        }))
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
                grant_exposure: registry_auth_grant_exposure(params.exposure),
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
            Ok(record) => {
                self.model_discovery
                    .invalidate_auth_provider(record.provider_id.as_str());
                Ok(AgentApiOutcome::new(AuthProviderCreateResponse {
                    provider: auth_provider_view(record),
                }))
            }
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
        self.model_discovery
            .invalidate_auth_provider(record.provider_id.as_str());
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
            registry_auth_grant_exposure(params.exposure),
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
    /// MCP server: protected resource metadata, authorization
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
/// Server-side id allocation for registered environments admitted on the
/// gateway's connect route.
pub(crate) fn allocate_environment_id_public() -> ::environments::EnvironmentId {
    environment_lifecycle::allocate_environment_id()
}

pub(crate) fn allocate_incarnation_id_public() -> ::environments::EnvironmentIncarnationId {
    environment_lifecycle::allocate_incarnation_id()
}

#[cfg(test)]
mod tests;

/// Deployment-scoped CIMD document: depends only on the public base URL, so
/// the multi-universe HTTP edge serves it without resolving a universe.
pub(crate) fn cimd_document_for(public_base_url: &str) -> serde_json::Value {
    oauth_api::cimd_document(public_base_url)
}
