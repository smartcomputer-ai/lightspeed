//! Client-facing API contracts for Lightspeed agents.
//!
//! This crate is intentionally independent of `engine` core types. Hosts
//! can implement these contracts from a local event-log runner, a Temporal
//! workflow gateway, or another substrate while clients keep speaking the same
//! session/run/item protocol.

use std::collections::BTreeMap;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

mod schema_export;
pub use schema_export::{ExportedSchemas, export_schemas};

pub const PROTOCOL_VERSION: &str = "lightspeed.agent.api.v1";

pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_SESSION_START: &str = "session/start";
pub const METHOD_SESSION_UPDATE: &str = "session/update";
pub const METHOD_SESSION_TOOLS_UPDATE: &str = "session/tools/update";
pub const METHOD_SESSION_READ: &str = "session/read";
pub const METHOD_SESSION_EVENTS_READ: &str = "session/events/read";
pub const METHOD_SESSION_CLOSE: &str = "session/close";
pub const METHOD_CONTEXT_COMPACT: &str = "context/compact";
pub const METHOD_CONTEXT_APPEND: &str = "context/append";
pub const METHOD_OUTBOX_READ: &str = "outbox/read";
pub const METHOD_OUTBOX_ACK: &str = "outbox/ack";
pub const METHOD_RUN_START: &str = "run/start";
pub const METHOD_RUN_CANCEL: &str = "run/cancel";
pub const METHOD_PROMPTS_ACTIVE: &str = "prompts/active";
pub const METHOD_SKILLS_LIST: &str = "skills/list";
pub const METHOD_SKILLS_ACTIVE: &str = "skills/active";
pub const METHOD_SKILLS_ACTIVATE: &str = "skills/activate";
pub const METHOD_SKILLS_DEACTIVATE: &str = "skills/deactivate";
pub const METHOD_SESSION_ENVIRONMENTS_LIST: &str = "session/environments/list";
pub const METHOD_SESSION_ENVIRONMENTS_READ: &str = "session/environments/read";
pub const METHOD_SESSION_ENVIRONMENTS_CREATE: &str = "session/environments/create";
pub const METHOD_SESSION_ENVIRONMENTS_ATTACH: &str = "session/environments/attach";
pub const METHOD_SESSION_ENVIRONMENTS_ACTIVATE: &str = "session/environments/activate";
pub const METHOD_SESSION_ENVIRONMENTS_DEACTIVATE: &str = "session/environments/deactivate";
pub const METHOD_SESSION_ENVIRONMENTS_CLOSE: &str = "session/environments/close";
pub const METHOD_ENVIRONMENT_PROVIDERS_REGISTER: &str = "environmentProviders/register";
pub const METHOD_ENVIRONMENT_PROVIDERS_HEARTBEAT: &str = "environmentProviders/heartbeat";
pub const METHOD_ENVIRONMENT_PROVIDERS_UNREGISTER: &str = "environmentProviders/unregister";
pub const METHOD_BLOB_PUT: &str = "blob/put";
pub const METHOD_BLOB_PUT_MANY: &str = "blob/put_many";
pub const METHOD_BLOB_GET: &str = "blob/get";
pub const METHOD_BLOB_HAS_MANY: &str = "blob/has_many";
pub const METHOD_VFS_SNAPSHOT_COMMIT: &str = "vfs/snapshot/commit";
pub const METHOD_VFS_SNAPSHOT_READ: &str = "vfs/snapshot/read";
pub const METHOD_VFS_WORKSPACE_CREATE: &str = "vfs/workspace/create";
pub const METHOD_VFS_WORKSPACE_READ: &str = "vfs/workspace/read";
pub const METHOD_VFS_WORKSPACE_UPDATE: &str = "vfs/workspace/update";
pub const METHOD_VFS_WORKSPACE_DELETE: &str = "vfs/workspace/delete";
pub const METHOD_VFS_MOUNT_PUT: &str = "vfs/mount/put";
pub const METHOD_VFS_MOUNT_LIST: &str = "vfs/mount/list";
pub const METHOD_VFS_MOUNT_DELETE: &str = "vfs/mount/delete";
pub const METHOD_MCP_SERVERS_CREATE: &str = "mcp/servers/create";
pub const METHOD_MCP_SERVERS_LIST: &str = "mcp/servers/list";
pub const METHOD_MCP_SERVERS_READ: &str = "mcp/servers/read";
pub const METHOD_MCP_SERVERS_DELETE: &str = "mcp/servers/delete";
pub const METHOD_SESSION_MCP_LINK: &str = "session/mcp/link";
pub const METHOD_SESSION_MCP_UNLINK: &str = "session/mcp/unlink";
pub const METHOD_SESSION_MCP_LIST: &str = "session/mcp/list";
pub const METHOD_AUTH_GRANTS_IMPORT: &str = "auth/grants/import";
pub const METHOD_AUTH_GRANTS_LIST: &str = "auth/grants/list";
pub const METHOD_AUTH_GRANTS_READ: &str = "auth/grants/read";
pub const METHOD_AUTH_GRANTS_REVOKE: &str = "auth/grants/revoke";
pub const METHOD_AUTH_CLIENTS_CREATE: &str = "auth/clients/create";
pub const METHOD_AUTH_CLIENTS_LIST: &str = "auth/clients/list";
pub const METHOD_AUTH_CLIENTS_READ: &str = "auth/clients/read";
pub const METHOD_AUTH_CLIENTS_DELETE: &str = "auth/clients/delete";
pub const METHOD_AUTH_FLOWS_START: &str = "auth/flows/start";
pub const METHOD_AUTH_FLOWS_STATUS: &str = "auth/flows/status";
pub const METHOD_AUTH_PROVIDERS_CREATE: &str = "auth/providers/create";
pub const METHOD_AUTH_PROVIDERS_LIST: &str = "auth/providers/list";
pub const METHOD_AUTH_PROVIDERS_READ: &str = "auth/providers/read";
pub const METHOD_AUTH_PROVIDERS_DELETE: &str = "auth/providers/delete";
pub const METHOD_AUTH_GITHUB_INSTALLATIONS_LIST: &str = "auth/github/installations/list";
pub const METHOD_AUTH_GITHUB_INSTALLATIONS_GRANT: &str = "auth/github/installations/grant";

pub const NOTIFY_SESSION_STARTED: &str = "session/started";
pub const NOTIFY_SESSION_STATUS_CHANGED: &str = "session/status/changed";
pub const NOTIFY_SESSION_EVENT: &str = "session/event";
pub const NOTIFY_RUN_STARTED: &str = "run/started";
pub const NOTIFY_RUN_COMPLETED: &str = "run/completed";
pub const NOTIFY_ITEM_COMPLETED: &str = "item/completed";
pub const NOTIFY_ERROR: &str = "error";

pub type SessionId = String;
pub type RunId = String;
pub type ItemId = String;
pub type SkillId = String;
pub type EnvironmentId = String;
pub type EnvironmentProviderId = String;
pub type EnvironmentTargetId = String;

const SESSION_ID_MAX_LEN: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SessionIdError {
    #[error("session id must not be empty")]
    Empty,
    #[error("session id is too long: {actual} bytes, max {max}")]
    TooLong { max: usize, actual: usize },
    #[error("session id must start with an ASCII letter or digit")]
    InvalidStart,
    #[error(
        "session id contains invalid character {ch:?} at byte {index}; allowed: ASCII letters, digits, '_', '-', '.', ':'"
    )]
    InvalidCharacter { index: usize, ch: char },
}

pub fn validate_session_id(value: &str) -> Result<(), SessionIdError> {
    if value.is_empty() {
        return Err(SessionIdError::Empty);
    }
    if value.len() > SESSION_ID_MAX_LEN {
        return Err(SessionIdError::TooLong {
            max: SESSION_ID_MAX_LEN,
            actual: value.len(),
        });
    }
    let Some(first) = value.chars().next() else {
        return Err(SessionIdError::Empty);
    };
    if !first.is_ascii_alphanumeric() {
        return Err(SessionIdError::InvalidStart);
    }
    for (index, ch) in value.char_indices() {
        if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':')) {
            return Err(SessionIdError::InvalidCharacter { index, ch });
        }
    }
    Ok(())
}

#[async_trait]
pub trait AgentApiService: Send + Sync {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> Result<AgentApiOutcome<InitializeResponse>, AgentApiError>;

    async fn start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError>;

    async fn update_session(
        &self,
        params: SessionUpdateParams,
    ) -> Result<AgentApiOutcome<SessionUpdateResponse>, AgentApiError>;

    async fn update_session_tools(
        &self,
        params: SessionToolsUpdateParams,
    ) -> Result<AgentApiOutcome<SessionToolsUpdateResponse>, AgentApiError>;

    async fn read_session(
        &self,
        params: SessionReadParams,
    ) -> Result<AgentApiOutcome<SessionReadResponse>, AgentApiError>;

    async fn read_session_events(
        &self,
        params: SessionEventsReadParams,
    ) -> Result<AgentApiOutcome<SessionEventsReadResponse>, AgentApiError>;

    async fn close_session(
        &self,
        params: SessionCloseParams,
    ) -> Result<AgentApiOutcome<SessionCloseResponse>, AgentApiError>;

    async fn compact_context(
        &self,
        params: ContextCompactParams,
    ) -> Result<AgentApiOutcome<ContextCompactResponse>, AgentApiError>;

    async fn append_context(
        &self,
        params: ContextAppendParams,
    ) -> Result<AgentApiOutcome<ContextAppendResponse>, AgentApiError>;

    async fn read_outbox(
        &self,
        params: OutboxReadParams,
    ) -> Result<AgentApiOutcome<OutboxReadResponse>, AgentApiError>;

    async fn ack_outbox(
        &self,
        params: OutboxAckParams,
    ) -> Result<AgentApiOutcome<OutboxAckResponse>, AgentApiError>;

    async fn start_run(
        &self,
        params: RunStartParams,
    ) -> Result<AgentApiOutcome<RunStartResponse>, AgentApiError>;

    async fn cancel_run(
        &self,
        params: RunCancelParams,
    ) -> Result<AgentApiOutcome<RunCancelResponse>, AgentApiError>;

    async fn active_prompts(
        &self,
        params: PromptsActiveParams,
    ) -> Result<AgentApiOutcome<PromptsActiveResponse>, AgentApiError>;

    async fn list_skills(
        &self,
        params: SkillListParams,
    ) -> Result<AgentApiOutcome<SkillListResponse>, AgentApiError>;

    async fn active_skills(
        &self,
        params: SkillActiveParams,
    ) -> Result<AgentApiOutcome<SkillActiveResponse>, AgentApiError>;

    async fn activate_skill(
        &self,
        params: SkillActivateParams,
    ) -> Result<AgentApiOutcome<SkillActivateResponse>, AgentApiError>;

    async fn deactivate_skill(
        &self,
        params: SkillDeactivateParams,
    ) -> Result<AgentApiOutcome<SkillDeactivateResponse>, AgentApiError>;

    async fn list_session_environments(
        &self,
        params: SessionEnvironmentListParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentListResponse>, AgentApiError>;

    async fn read_session_environment(
        &self,
        params: SessionEnvironmentReadParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentReadResponse>, AgentApiError>;

    async fn create_session_environment(
        &self,
        params: SessionEnvironmentCreateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentCreateResponse>, AgentApiError>;

    async fn attach_session_environment(
        &self,
        params: SessionEnvironmentAttachParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentAttachResponse>, AgentApiError>;

    async fn activate_session_environment(
        &self,
        params: SessionEnvironmentActivateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentActivateResponse>, AgentApiError>;

    async fn deactivate_session_environment(
        &self,
        params: SessionEnvironmentDeactivateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentDeactivateResponse>, AgentApiError>;

    async fn close_session_environment(
        &self,
        params: SessionEnvironmentCloseParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentCloseResponse>, AgentApiError>;

    async fn register_environment_provider(
        &self,
        params: EnvironmentProviderRegisterParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderRegisterResponse>, AgentApiError>;

    async fn heartbeat_environment_provider(
        &self,
        params: EnvironmentProviderHeartbeatParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderHeartbeatResponse>, AgentApiError>;

    async fn unregister_environment_provider(
        &self,
        params: EnvironmentProviderUnregisterParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderUnregisterResponse>, AgentApiError>;

    async fn put_blob(
        &self,
        params: BlobPutParams,
    ) -> Result<AgentApiOutcome<BlobPutResponse>, AgentApiError>;

    async fn put_blobs(
        &self,
        params: BlobPutManyParams,
    ) -> Result<AgentApiOutcome<BlobPutManyResponse>, AgentApiError>;

    async fn get_blob(
        &self,
        params: BlobGetParams,
    ) -> Result<AgentApiOutcome<BlobGetResponse>, AgentApiError>;

    async fn has_blobs(
        &self,
        params: BlobHasManyParams,
    ) -> Result<AgentApiOutcome<BlobHasManyResponse>, AgentApiError>;

    async fn commit_vfs_snapshot(
        &self,
        params: VfsSnapshotCommitParams,
    ) -> Result<AgentApiOutcome<VfsSnapshotCommitResponse>, AgentApiError>;

    async fn read_vfs_snapshot(
        &self,
        params: VfsSnapshotReadParams,
    ) -> Result<AgentApiOutcome<VfsSnapshotReadResponse>, AgentApiError>;

    async fn create_vfs_workspace(
        &self,
        params: VfsWorkspaceCreateParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceCreateResponse>, AgentApiError>;

    async fn read_vfs_workspace(
        &self,
        params: VfsWorkspaceReadParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceReadResponse>, AgentApiError>;

    async fn update_vfs_workspace(
        &self,
        params: VfsWorkspaceUpdateParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceUpdateResponse>, AgentApiError>;

    async fn delete_vfs_workspace(
        &self,
        params: VfsWorkspaceDeleteParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceDeleteResponse>, AgentApiError>;

    async fn put_vfs_mount(
        &self,
        params: VfsMountPutParams,
    ) -> Result<AgentApiOutcome<VfsMountPutResponse>, AgentApiError>;

    async fn delete_vfs_mount(
        &self,
        params: VfsMountDeleteParams,
    ) -> Result<AgentApiOutcome<VfsMountDeleteResponse>, AgentApiError>;

    async fn list_vfs_mounts(
        &self,
        params: VfsMountListParams,
    ) -> Result<AgentApiOutcome<VfsMountListResponse>, AgentApiError>;

    async fn create_mcp_server(
        &self,
        params: McpServerCreateParams,
    ) -> Result<AgentApiOutcome<McpServerCreateResponse>, AgentApiError>;

    async fn list_mcp_servers(
        &self,
        params: McpServerListParams,
    ) -> Result<AgentApiOutcome<McpServerListResponse>, AgentApiError>;

    async fn read_mcp_server(
        &self,
        params: McpServerReadParams,
    ) -> Result<AgentApiOutcome<McpServerReadResponse>, AgentApiError>;

    async fn delete_mcp_server(
        &self,
        params: McpServerDeleteParams,
    ) -> Result<AgentApiOutcome<McpServerDeleteResponse>, AgentApiError>;

    async fn link_session_mcp(
        &self,
        params: SessionMcpLinkParams,
    ) -> Result<AgentApiOutcome<SessionMcpLinkResponse>, AgentApiError>;

    async fn unlink_session_mcp(
        &self,
        params: SessionMcpUnlinkParams,
    ) -> Result<AgentApiOutcome<SessionMcpUnlinkResponse>, AgentApiError>;

    async fn list_session_mcp(
        &self,
        params: SessionMcpListParams,
    ) -> Result<AgentApiOutcome<SessionMcpListResponse>, AgentApiError>;

    async fn import_auth_grant(
        &self,
        params: AuthGrantImportParams,
    ) -> Result<AgentApiOutcome<AuthGrantImportResponse>, AgentApiError>;

    async fn list_auth_grants(
        &self,
        params: AuthGrantListParams,
    ) -> Result<AgentApiOutcome<AuthGrantListResponse>, AgentApiError>;

    async fn read_auth_grant(
        &self,
        params: AuthGrantReadParams,
    ) -> Result<AgentApiOutcome<AuthGrantReadResponse>, AgentApiError>;

    async fn revoke_auth_grant(
        &self,
        params: AuthGrantRevokeParams,
    ) -> Result<AgentApiOutcome<AuthGrantRevokeResponse>, AgentApiError>;

    async fn create_auth_client(
        &self,
        params: AuthClientCreateParams,
    ) -> Result<AgentApiOutcome<AuthClientCreateResponse>, AgentApiError>;

    async fn list_auth_clients(
        &self,
        params: AuthClientListParams,
    ) -> Result<AgentApiOutcome<AuthClientListResponse>, AgentApiError>;

    async fn read_auth_client(
        &self,
        params: AuthClientReadParams,
    ) -> Result<AgentApiOutcome<AuthClientReadResponse>, AgentApiError>;

    async fn delete_auth_client(
        &self,
        params: AuthClientDeleteParams,
    ) -> Result<AgentApiOutcome<AuthClientDeleteResponse>, AgentApiError>;

    async fn start_auth_flow(
        &self,
        params: AuthFlowStartParams,
    ) -> Result<AgentApiOutcome<AuthFlowStartResponse>, AgentApiError>;

    async fn read_auth_flow_status(
        &self,
        params: AuthFlowStatusParams,
    ) -> Result<AgentApiOutcome<AuthFlowStatusResponse>, AgentApiError>;

    async fn create_auth_provider(
        &self,
        params: AuthProviderCreateParams,
    ) -> Result<AgentApiOutcome<AuthProviderCreateResponse>, AgentApiError>;

    async fn list_auth_providers(
        &self,
        params: AuthProviderListParams,
    ) -> Result<AgentApiOutcome<AuthProviderListResponse>, AgentApiError>;

    async fn read_auth_provider(
        &self,
        params: AuthProviderReadParams,
    ) -> Result<AgentApiOutcome<AuthProviderReadResponse>, AgentApiError>;

    async fn delete_auth_provider(
        &self,
        params: AuthProviderDeleteParams,
    ) -> Result<AgentApiOutcome<AuthProviderDeleteResponse>, AgentApiError>;

    async fn list_github_installations(
        &self,
        params: AuthGitHubInstallationListParams,
    ) -> Result<AgentApiOutcome<AuthGitHubInstallationListResponse>, AgentApiError>;

    async fn grant_github_installation(
        &self,
        params: AuthGitHubInstallationGrantParams,
    ) -> Result<AgentApiOutcome<AuthGitHubInstallationGrantResponse>, AgentApiError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename = "AgentApiOutcomeOf{T}")]
pub struct AgentApiOutcome<T> {
    pub result: T,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notifications: Vec<AgentNotification>,
}

impl<T> AgentApiOutcome<T> {
    pub fn new(result: T) -> Self {
        Self {
            result,
            notifications: Vec::new(),
        }
    }

    pub fn with_notifications(result: T, notifications: Vec<AgentNotification>) -> Self {
        Self {
            result,
            notifications,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: Option<ClientInfo>,
    pub capabilities: Option<ClientCapabilities>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(default)]
    pub experimental_api: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: String,
    pub server_info: ServerInfo,
    pub capabilities: ServerCapabilities,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    pub notifications: bool,
    pub history_read: bool,
    #[serde(default)]
    pub event_log: bool,
    pub local_execution: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartParams {
    pub session_id: Option<SessionId>,
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<SessionConfigInput>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextConfigInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_defaults: Option<RunDefaultsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolConfigInput>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoiceConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolChoiceConfig {
    pub mode: ToolChoiceModeConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_parallel_tool_use: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ToolChoiceModeConfig {
    Auto,
    None,
    RequiredAny,
    Specific { tool_id: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextConfigInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionPolicyInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "mode")]
pub enum CompactionPolicyInput {
    Disabled,
    ProviderTriggered {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compact_threshold_tokens: Option<u32>,
    },
    ProviderStandalone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compact_threshold_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_tokens: Option<u32>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunDefaultsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfigInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_fetch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FilesystemToolMode>,
    /// Enables the messaging toolset (message_send/react/edit/noop) for
    /// sessions bound to a chat channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging: Option<bool>,
    /// Enables the Fleet subagent control-plane tools (agent_spawn/send/read/list/cancel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FilesystemToolMode {
    None,
    ReadOnly,
    Edit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdateParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_config_revision: Option<u64>,
    #[serde(default)]
    pub patch: SessionConfigPatchInput,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigPatchInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationConfigPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextConfigPatchInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_defaults: Option<RunDefaultsPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolConfigPatchInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "op", content = "value")]
#[schemars(rename = "FieldPatchOf{T}")]
pub enum FieldPatch<T> {
    Set(T),
    Clear,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfigPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<FieldPatch<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<FieldPatch<ToolChoiceConfig>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextConfigPatchInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<FieldPatch<CompactionPolicyInput>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunDefaultsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<FieldPatch<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<FieldPatch<u32>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfigPatchInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<FieldPatch<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_fetch: Option<FieldPatch<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FieldPatch<FilesystemToolMode>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging: Option<FieldPatch<bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet: Option<FieldPatch<bool>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdateResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionToolsUpdateParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_tools_revision: Option<u64>,
    pub update: SessionToolsUpdateInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionToolsUpdateInput {
    Replace {
        #[serde(default)]
        tools: Vec<ToolView>,
    },
    Patch {
        #[serde(default)]
        upsert: Vec<ToolView>,
        #[serde(default)]
        remove: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionToolsUpdateResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActiveToolsView {
    pub revision: u64,
    #[serde(default)]
    pub tools: Vec<ToolView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolView {
    pub tool_id: String,
    pub kind: ToolKindView,
    #[serde(default)]
    pub parallelism: ToolParallelismView,
    #[serde(default)]
    pub target_requirement: ToolTargetRequirementView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ToolKindView {
    Function {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description_ref: Option<String>,
        input_schema_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_schema_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_options_ref: Option<String>,
    },
    ProviderNative {
        api_kind: String,
        native_tool_ref: String,
        execution: ProviderNativeToolExecutionView,
    },
    RemoteMcp {
        server_label: String,
        server_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_tools: Option<Vec<String>>,
        #[serde(default)]
        approval: RemoteMcpApprovalPolicy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_ref: Option<SecretRefView>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ProviderNativeToolExecutionView {
    #[default]
    ProviderHosted,
    ClientEffect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToolParallelismView {
    Exclusive,
    #[default]
    ParallelSafe,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ToolTargetRequirementView {
    #[default]
    None,
    Optional {
        namespace: String,
    },
    Required {
        namespace: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextCompactParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextCompactResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendParams {
    pub session_id: SessionId,
    pub entries: Vec<ContextAppendEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendEntry {
    /// Stable client-chosen context key. Re-sending the same key with the
    /// same content is a no-op, so the key doubles as the idempotency handle.
    pub key: String,
    pub item: InputItem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendResponse {
    pub context_revision: u64,
    pub applied_keys: Vec<String>,
    pub unchanged_keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutboxReadParams {
    /// Return pending entries with `seq` greater than this cursor. Restart
    /// from 0 to re-read undelivered entries after a consumer restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Long-poll wait in milliseconds when no entries are pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutboxReadResponse {
    pub entries: Vec<OutboundMessageView>,
    /// Cursor to pass as `after` on the next read.
    pub next_after: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutboundMessageView {
    pub seq: u64,
    pub outbox_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub origin: OutboundOriginView,
    pub payload: OutboundPayloadView,
    pub attempts: u32,
    pub created_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum OutboundOriginView {
    ToolCall,
    FinalText,
    Trigger,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OutboundPayloadView {
    Send {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
    },
    React {
        message_id: String,
        emoji: String,
    },
    Edit {
        message_id: String,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutboxAckParams {
    pub outbox_id: String,
    pub result: OutboundAckInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OutboundAckInput {
    Delivered {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel_message_id: Option<String>,
    },
    Failed {
        error: String,
        retryable: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutboxAckResponse {
    pub outbox_id: String,
    pub status: OutboundStatusView,
    pub attempts: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum OutboundStatusView {
    Pending,
    Delivered,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventsReadParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<EventCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Long-poll: when no events exist past `after`, hold the request until
    /// one lands or this many milliseconds elapse, then return a normal
    /// (possibly empty) page. Zero or absent preserves immediate return.
    /// Values above the server cap are clamped, not rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventsReadResponse {
    #[serde(default)]
    pub events: Vec<SessionEventView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<EventCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_cursor: Option<EventCursor>,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<EventLogGap>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloseParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloseResponse {
    pub session: SessionView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventCursor {
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventLogGap {
    pub requested_after: Option<EventCursor>,
    pub retained_after: Option<EventCursor>,
    pub next_cursor: Option<EventCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventView {
    pub cursor: EventCursor,
    pub session_id: SessionId,
    pub observed_at_ms: u64,
    pub joins: EventJoinsView,
    pub kind: SessionEventKindView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventJoinsView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionEventKindView {
    SessionOpened {
        model: Option<ModelConfig>,
    },
    SessionConfigChanged {
        model: Option<ModelConfig>,
        revision: u64,
    },
    SessionClosed,
    RunAccepted {
        run_id: RunId,
        submission_id: Option<String>,
        input: Vec<ContextEntryInputView>,
    },
    RunStarted {
        run_id: RunId,
    },
    RunSteeringAccepted {
        run_id: RunId,
        steering_id: String,
        input: Vec<ContextEntryInputView>,
    },
    RunCancellationRequested {
        run_id: RunId,
    },
    RunCompleted {
        run_id: RunId,
        output_ref: Option<String>,
    },
    RunFailed {
        run_id: RunId,
        message: String,
    },
    RunCancelled {
        run_id: RunId,
    },
    TurnStarted {
        run_id: RunId,
        turn_id: String,
    },
    TurnPlanned {
        run_id: RunId,
        turn_id: String,
    },
    TurnGenerationRequested {
        run_id: RunId,
        turn_id: String,
    },
    TurnGenerationCompleted {
        run_id: RunId,
        turn_id: String,
        status: String,
    },
    TurnCompleted {
        turn_id: String,
    },
    ContextEntriesApplied {
        base_revision: u64,
        revision: u64,
        items: Vec<SessionItemView>,
    },
    ContextEntriesRemoved {
        base_revision: u64,
        revision: u64,
        item_ids: Vec<ItemId>,
        reason: String,
    },
    ContextKeysRemoved {
        base_revision: u64,
        revision: u64,
        keys: Vec<String>,
    },
    ContextKeyPrefixReplaced {
        base_revision: u64,
        revision: u64,
        key_prefix: String,
        items: Vec<SessionItemView>,
    },
    ContextStateReplaced {
        base_revision: u64,
        revision: u64,
        items: Vec<SessionItemView>,
        reason: String,
    },
    ContextCompactionRequested {
        base_revision: u64,
        revision: u64,
        trigger: String,
    },
    ContextCompactionFinished {
        base_revision: u64,
        revision: u64,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_ref: Option<String>,
    },
    SkillCatalogSet {
        catalog_ref: Option<String>,
    },
    SkillActivationsSet {
        skill_ids: Vec<String>,
    },
    ToolsReplaced {
        base_revision: u64,
        revision: u64,
    },
    ToolsPatched {
        base_revision: u64,
        revision: u64,
        upserted: Vec<String>,
        removed: Vec<String>,
    },
    ToolDefaultTargetChanged {
        namespace: String,
        target: Option<ToolExecutionTargetView>,
    },
    ToolBatchStarted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        calls: Vec<ToolCallEventView>,
    },
    ToolCallStarted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        call_id: String,
    },
    ToolCallCompleted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        call_id: String,
        status: ToolItemStatus,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        effects: Vec<ToolEffectView>,
    },
    ToolBatchDeferred {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
    },
    ToolBatchResumed {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
    },
    ToolBatchCompleted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionTargetView {
    pub namespace: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEventView {
    pub call_id: String,
    pub tool_name: String,
    pub arguments_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<ToolCallDisplayView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStartParams {
    pub session_id: SessionId,
    pub input: Vec<InputItem>,
    /// Client-supplied idempotency key, unique per session. Retrying
    /// `run/start` with the same submission id and the same input/config
    /// returns the original run instead of starting a second one; reusing a
    /// submission id with different input or config is rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<RunStartConfig>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStartConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<RunLimitsConfig>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunLimitsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStartResponse {
    pub run: RunView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunCancelParams {
    pub session_id: SessionId,
    pub run_id: RunId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunCancelResponse {
    pub run: RunView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromptsActiveParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromptsActiveResponse {
    #[serde(default)]
    pub instructions: Vec<PromptInstructionView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromptInstructionView {
    pub key: String,
    pub instructions_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillListParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillListResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_ref: Option<String>,
    #[serde(default)]
    pub skills: Vec<SkillListItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillListItem {
    pub skill_id: SkillId,
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    pub enabled: bool,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillActiveParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillActiveResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_ref: Option<String>,
    #[serde(default)]
    pub activations: Vec<SkillActivationView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillActivationView {
    pub skill_id: SkillId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    pub catalog_ref: String,
    pub scope: SkillActivationScope,
    pub source: SkillActivationSource,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SkillActivationScope {
    #[default]
    Run,
    Session,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SkillActivationSource {
    ToolResult { call_id: String },
    DirectContext { context_ref: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillActivateParams {
    pub session_id: SessionId,
    pub skill_id: SkillId,
    #[serde(default)]
    pub scope: SkillActivationScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillActivateResponse {
    pub activation: SkillActivationView,
    #[serde(default)]
    pub active: Vec<SkillActivationView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillDeactivateParams {
    pub session_id: SessionId,
    pub skill_id: SkillId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillDeactivateResponse {
    pub skill_id: SkillId,
    #[serde(default)]
    pub active: Vec<SkillActivationView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentView {
    pub env_id: EnvironmentId,
    pub kind: SessionEnvironmentKindView,
    pub status: SessionEnvironmentStatusView,
    pub capabilities: SessionEnvironmentCapabilitiesView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_target: Option<ToolExecutionTargetView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionEnvironmentKindView {
    Sandbox,
    AttachedHost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionEnvironmentStatusView {
    Attaching,
    Ready,
    Degraded,
    Detached,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentCapabilitiesView {
    pub fs_read: bool,
    pub fs_write: bool,
    pub process_exec: bool,
    pub process_stdin: bool,
    pub network: bool,
    pub persistent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentListParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentListResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env_id: Option<EnvironmentId>,
    #[serde(default)]
    pub environments: Vec<SessionEnvironmentView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentReadParams {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentReadResponse {
    pub environment: SessionEnvironmentView,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentCreateParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<EnvironmentId>,
    pub provider_id: EnvironmentProviderId,
    pub request: HostTargetCreateRequestView,
    #[serde(default)]
    pub activate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentCreateResponse {
    pub environment: SessionEnvironmentView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env_id: Option<EnvironmentId>,
    #[serde(default)]
    pub environments: Vec<SessionEnvironmentView>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentAttachParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<EnvironmentId>,
    pub provider_id: EnvironmentProviderId,
    pub request: HostTargetAttachRequestView,
    #[serde(default)]
    pub activate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentAttachResponse {
    pub environment: SessionEnvironmentView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env_id: Option<EnvironmentId>,
    #[serde(default)]
    pub environments: Vec<SessionEnvironmentView>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostTargetCreateRequestView {
    Sandbox { spec: SandboxTargetSpecView },
    AttachedHost { spec: AttachedHostSpecView },
    Provider { provider_type: String, spec: Value },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SandboxTargetSpecView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttachedHostSpecView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostTargetAttachRequestView {
    Target { target_id: EnvironmentTargetId },
    Provider { provider_type: String, spec: Value },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentActivateParams {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentActivateResponse {
    pub environment: SessionEnvironmentView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env_id: Option<EnvironmentId>,
    #[serde(default)]
    pub environments: Vec<SessionEnvironmentView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentDeactivateParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentDeactivateResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env_id: Option<EnvironmentId>,
    #[serde(default)]
    pub environments: Vec<SessionEnvironmentView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentCloseParams {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_target: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentCloseResponse {
    pub environment: SessionEnvironmentView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env_id: Option<EnvironmentId>,
    #[serde(default)]
    pub environments: Vec<SessionEnvironmentView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderView {
    pub provider_id: EnvironmentProviderId,
    pub provider_kind: EnvironmentProviderKindView,
    pub status: EnvironmentProviderStatusView,
    pub controller_connection: HostControllerConnectionView,
    pub capabilities: EnvironmentProviderCapabilitiesView,
    pub implementation: EnvironmentProviderImplementationView,
    pub last_seen_ms: i64,
    pub lease_expires_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentProviderKindView {
    Sandbox,
    Bridge,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentProviderStatusView {
    Registering,
    Online,
    Stale,
    Offline,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostControllerConnectionView {
    pub endpoint: String,
    pub transport: HostTransportView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostTransportView {
    WebSocket,
    Http,
    Stdio,
    Ssh,
    Provider { provider_type: String },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderCapabilitiesView {
    #[serde(default)]
    pub list_targets: bool,
    #[serde(default)]
    pub create_target: bool,
    #[serde(default)]
    pub attach_target: bool,
    #[serde(default)]
    pub get_target: bool,
    #[serde(default)]
    pub close_target: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderImplementationView {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTargetSummaryView {
    pub target_id: EnvironmentTargetId,
    pub status: EnvironmentTargetStatusView,
    pub scope: HostScopeView,
    pub capabilities: HostCapabilitiesView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentTargetStatusView {
    Creating,
    Starting,
    Ready,
    Stopped,
    Closing,
    Closed,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostScopeView {
    Default,
    Session { session_id: String },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostCapabilitiesView {
    #[serde(default)]
    pub filesystem_read: bool,
    #[serde(default)]
    pub filesystem_write: bool,
    #[serde(default)]
    pub process_start: bool,
    #[serde(default)]
    pub process_stdin: bool,
    #[serde(default)]
    pub process_terminate: bool,
    #[serde(default)]
    pub process_output_polling: bool,
    #[serde(default)]
    pub process_output_notifications: bool,
    #[serde(default)]
    pub process_pty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderRegisterParams {
    pub provider_id: EnvironmentProviderId,
    pub provider_kind: EnvironmentProviderKindView,
    pub controller_connection: HostControllerConnectionView,
    pub capabilities: EnvironmentProviderCapabilitiesView,
    pub implementation: EnvironmentProviderImplementationView,
    pub lease_ttl_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderRegisterResponse {
    pub provider: EnvironmentProviderView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderHeartbeatParams {
    pub provider_id: EnvironmentProviderId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ttl_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed_targets: Vec<EnvironmentTargetSummaryView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderHeartbeatResponse {
    pub provider: EnvironmentProviderView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<EnvironmentTargetSummaryView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderUnregisterParams {
    pub provider_id: EnvironmentProviderId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderUnregisterResponse {
    pub provider: EnvironmentProviderView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobPutParams {
    pub bytes_base64: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobPutResponse {
    pub blob_ref: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobPutManyParams {
    #[serde(default)]
    pub blobs: Vec<BlobPutParams>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobPutManyResponse {
    #[serde(default)]
    pub blobs: Vec<BlobPutResponse>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobGetParams {
    pub blob_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobGetResponse {
    pub blob_ref: String,
    pub bytes_base64: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobHasManyParams {
    #[serde(default)]
    pub blob_refs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobHasItem {
    pub blob_ref: String,
    pub exists: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobHasManyResponse {
    #[serde(default)]
    pub blobs: Vec<BlobHasItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsSnapshotCommitParams {
    pub manifest: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsSnapshotCommitResponse {
    pub snapshot_ref: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsSnapshotReadParams {
    pub snapshot_ref: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsSnapshotReadResponse {
    pub snapshot_ref: String,
    pub manifest: Value,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub snapshot_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceCreateResponse {
    pub workspace: VfsWorkspaceView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceReadParams {
    pub workspace_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceReadResponse {
    pub workspace: VfsWorkspaceView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceUpdateParams {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    pub snapshot_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceUpdateResponse {
    pub workspace: VfsWorkspaceView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceDeleteParams {
    pub workspace_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceDeleteResponse {
    pub workspace: VfsWorkspaceView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceView {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_snapshot_ref: Option<String>,
    pub head_snapshot_ref: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VfsMountSourceInput {
    Snapshot { snapshot_ref: String },
    Workspace { workspace_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum VfsMountSourceView {
    Snapshot {
        snapshot_ref: String,
    },
    Workspace {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head_snapshot_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum VfsMountAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsMountView {
    pub mount_path: String,
    pub source: VfsMountSourceView,
    pub access: VfsMountAccess,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsMountPutParams {
    pub session_id: SessionId,
    pub mount_path: String,
    pub source: VfsMountSourceInput,
    pub access: VfsMountAccess,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsMountPutResponse {
    pub mount: VfsMountView,
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsMountDeleteParams {
    pub session_id: SessionId,
    pub mount_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsMountDeleteResponse {
    pub mount_path: String,
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsMountListParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsMountListResponse {
    #[serde(default)]
    pub mounts: Vec<VfsMountView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerView {
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub server_url: String,
    pub transport: RemoteMcpTransport,
    pub default_server_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    pub approval_default: RemoteMcpApprovalPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading_default: Option<bool>,
    pub auth_policy: McpServerAuthPolicy,
    pub status: McpServerStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RemoteMcpTransport {
    StreamableHttp,
    Sse,
    #[default]
    Auto,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RemoteMcpApprovalPolicy {
    ProviderDefault,
    Always,
    #[default]
    Never,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum McpServerAuthPolicy {
    #[default]
    None,
    OptionalBearer,
    RequiredBearer,
    OptionalOAuth {
        resource: String,
        #[serde(default)]
        scopes_default: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protected_resource_metadata_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authorization_server: Option<String>,
    },
    RequiredOAuth {
        resource: String,
        #[serde(default)]
        scopes_default: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protected_resource_metadata_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authorization_server: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum McpServerStatus {
    #[default]
    Active,
    NeedsAuthConfig,
    Unverified,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerCreateParams {
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub server_url: String,
    #[serde(default)]
    pub transport: RemoteMcpTransport,
    pub default_server_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub approval_default: RemoteMcpApprovalPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading_default: Option<bool>,
    #[serde(default)]
    pub auth_policy: McpServerAuthPolicy,
    #[serde(default)]
    pub status: McpServerStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerCreateResponse {
    pub server: McpServerView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<McpServerStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerListResponse {
    #[serde(default)]
    pub servers: Vec<McpServerView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerReadParams {
    pub server_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerReadResponse {
    pub server: McpServerView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerDeleteParams {
    pub server_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerDeleteResponse {
    pub server: McpServerView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMcpLinkView {
    pub tool_id: String,
    pub server_label: String,
    pub server_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    pub approval: RemoteMcpApprovalPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_ref: Option<SecretRefView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecretRefView {
    pub namespace: String,
    pub id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AuthProviderKind {
    StaticBearer,
    McpOAuth,
    GitHubApp,
    GitHubAppUser,
    GitHubOAuthApp,
    CustomOAuth,
    ModelApiKey,
    ModelOAuth,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AuthGrantStatus {
    #[default]
    Active,
    NeedsReauth,
    Revoked,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PrincipalKind {
    User,
    ServiceAccount,
    #[default]
    UniverseDefault,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrincipalRefView {
    #[serde(default)]
    pub kind: PrincipalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantView {
    pub grant_id: String,
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    pub principal: PrincipalRefView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    pub status: AuthGrantStatus,
    /// Non-secret provider-specific metadata (for GitHub App installation
    /// grants: installation id, account, permissions, repository selection).
    #[serde(default, skip_serializing_if = "metadata_is_empty")]
    pub metadata: serde_json::Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

fn metadata_is_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// Import a static bearer credential as an auth grant. This is the one
/// deliberate inbound-plaintext path: `token` is encrypted on receipt and is
/// never returned by any method. `Debug` output redacts the token; request
/// logging must never echo these params.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantImportParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}

impl std::fmt::Debug for AuthGrantImportParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthGrantImportParams")
            .field("grant_id", &self.grant_id)
            .field("provider_id", &self.provider_id)
            .field("token", &"<redacted>")
            .field("display_name", &self.display_name)
            .field("subject_hint", &self.subject_hint)
            .field("scopes", &self.scopes)
            .field("audience", &self.audience)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantImportResponse {
    pub grant: AuthGrantView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AuthGrantStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantListResponse {
    #[serde(default)]
    pub grants: Vec<AuthGrantView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantReadParams {
    pub grant_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantReadResponse {
    pub grant: AuthGrantView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantRevokeParams {
    pub grant_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantRevokeResponse {
    pub grant: AuthGrantView,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TokenEndpointAuthMethod {
    #[default]
    ClientSecretBasic,
    ClientSecretPost,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClientView {
    pub client_id: String,
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub remote_client_id: String,
    pub has_client_secret: bool,
    pub token_endpoint_auth_method: TokenEndpointAuthMethod,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes_default: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Register an OAuth client configuration. `client_secret` is the second
/// deliberate inbound-plaintext path after `auth/grants/import`: it is
/// encrypted on receipt and never returned by any method. `Debug` output
/// redacts it; request logging must never echo these params.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub provider_kind: AuthProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub remote_client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_endpoint_auth_method: Option<TokenEndpointAuthMethod>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes_default: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}

impl std::fmt::Debug for AuthClientCreateParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthClientCreateParams")
            .field("client_id", &self.client_id)
            .field("provider_id", &self.provider_id)
            .field("provider_kind", &self.provider_kind)
            .field("display_name", &self.display_name)
            .field("authorization_endpoint", &self.authorization_endpoint)
            .field("token_endpoint", &self.token_endpoint)
            .field("remote_client_id", &self.remote_client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "token_endpoint_auth_method",
                &self.token_endpoint_auth_method,
            )
            .field("scopes_default", &self.scopes_default)
            .field("audience", &self.audience)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientCreateResponse {
    pub client: OAuthClientView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientListResponse {
    #[serde(default)]
    pub clients: Vec<OAuthClientView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientReadParams {
    pub client_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientReadResponse {
    pub client: OAuthClientView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientDeleteParams {
    pub client_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientDeleteResponse {
    pub client: OAuthClientView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AuthFlowStatus {
    Pending,
    Completed,
    Failed,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthFlowStartParams {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthFlowStartResponse {
    pub flow_id: String,
    /// Authorization URL the user must open. It embeds the one-time `state`;
    /// treat it as sensitive and do not log it server-side.
    pub authorize_url: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthFlowStatusParams {
    pub flow_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthFlowView {
    pub flow_id: String,
    pub client_id: String,
    pub provider_id: String,
    pub status: AuthFlowStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub expires_at_ms: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthFlowStatusResponse {
    pub flow: AuthFlowView,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AuthProviderStatus {
    #[default]
    Active,
    NeedsConfiguration,
    Disabled,
}

/// Non-secret, provider-specific configuration. New providers add a
/// variant, not a table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum AuthProviderConfigView {
    #[serde(rename = "githubApp", rename_all = "camelCase")]
    GitHubApp {
        app_id: String,
        api_base_url: String,
    },
    /// Stored model provider API key (`model:<provider_id>` rows). The key itself
    /// is the provider credential and never appears in views.
    #[serde(rename = "modelApiKey", rename_all = "camelCase")]
    ModelApiKey {},
    /// OAuth-grant-backed model provider credential: provider calls send the
    /// bound grant's access token as an OAuth bearer token.
    #[serde(rename = "modelOAuth", rename_all = "camelCase")]
    ModelOAuth {
        grant_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audience: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum AuthProviderConfigInput {
    #[serde(rename = "githubApp", rename_all = "camelCase")]
    GitHubApp {
        app_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_base_url: Option<String>,
    },
    /// Stored model provider API key; the key arrives via `credential` and is
    /// encrypted on receipt.
    #[serde(rename = "modelApiKey", rename_all = "camelCase")]
    ModelApiKey {},
    /// Bind an existing auth grant as a model provider credential. No
    /// `credential` is accepted; the grant's tokens stay in the grant store.
    #[serde(rename = "modelOAuth", rename_all = "camelCase")]
    ModelOAuth {
        grant_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audience: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderView {
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub config: AuthProviderConfigView,
    pub has_credential: bool,
    pub status: AuthProviderStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Register an auth provider. `credential` (for GitHub Apps: the private
/// key PEM) is the third deliberate inbound-plaintext path: it is encrypted
/// on receipt and never returned by any method. `Debug` output redacts it;
/// request logging must never echo these params.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub config: AuthProviderConfigInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

impl std::fmt::Debug for AuthProviderCreateParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthProviderCreateParams")
            .field("provider_id", &self.provider_id)
            .field("display_name", &self.display_name)
            .field("config", &self.config)
            .field(
                "credential",
                &self.credential.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderCreateResponse {
    pub provider: AuthProviderView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderListResponse {
    #[serde(default)]
    pub providers: Vec<AuthProviderView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderReadParams {
    pub provider_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderReadResponse {
    pub provider: AuthProviderView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderDeleteParams {
    pub provider_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderDeleteResponse {
    pub provider: AuthProviderView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitHubInstallationView {
    pub installation_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_login: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_selection: Option<String>,
    /// Fine-grained permission map as GitHub reports it.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub permissions: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGitHubInstallationListParams {
    pub provider_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGitHubInstallationListResponse {
    #[serde(default)]
    pub installations: Vec<GitHubInstallationView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGitHubInstallationGrantParams {
    pub provider_id: String,
    pub installation_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGitHubInstallationGrantResponse {
    pub grant: AuthGrantView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMcpLinkParams {
    pub session_id: SessionId,
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<RemoteMcpApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_grant_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMcpLinkResponse {
    pub link: SessionMcpLinkView,
    #[serde(default)]
    pub links: Vec<SessionMcpLinkView>,
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMcpUnlinkParams {
    pub session_id: SessionId,
    pub tool_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMcpUnlinkResponse {
    pub tool_id: String,
    #[serde(default)]
    pub links: Vec<SessionMcpLinkView>,
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMcpListParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMcpListResponse {
    #[serde(default)]
    pub links: Vec<SessionMcpLinkView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    pub provider_id: String,
    pub api_kind: String,
    pub model: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: SessionId,
    pub status: SessionStatus,
    pub cwd: Option<String>,
    pub config_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<SessionConfigView>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub runs: Vec<RunView>,
    pub active_context: ContextView,
    #[serde(default)]
    pub active_tools: ActiveToolsView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vfs_mounts: Vec<VfsMountView>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextView {
    pub revision: u64,
    #[serde(default)]
    pub items: Vec<SessionItemView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigView {
    pub model: ModelConfig,
    pub generation: GenerationConfig,
    pub context: ContextConfigInput,
    pub run_defaults: RunDefaultsConfig,
    pub tools: ToolConfigView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfigView {
    pub web_search: bool,
    pub web_fetch: bool,
    pub filesystem: FilesystemToolMode,
    pub fleet: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    NotLoaded,
    Idle,
    Active,
    Closed,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub id: RunId,
    pub status: RunStatus,
    pub input: Vec<InputItem>,
    #[serde(default)]
    pub items: Vec<SessionItemView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_batches: Vec<ToolBatchView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolBatchView {
    pub id: String,
    pub turn_id: String,
    pub status: ToolItemStatus,
    #[serde(default)]
    pub calls: Vec<ToolCallView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallView {
    pub call_id: String,
    pub tool_name: String,
    pub arguments_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    pub status: ToolItemStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<ToolEffectView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<ToolCallDisplayView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolEffectView {
    pub kind: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallDisplayView {
    pub group: ToolCallDisplayGroup,
    pub verb: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallDisplayGroup {
    Explore,
    Edit,
    Execute,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderContextDisplayView {
    pub summary: ToolCallDisplayView,
    pub tool_name: String,
    pub status: ToolItemStatus,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RunStatus {
    Queued,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum InputItem {
    Text {
        text: String,
    },
    TextRef {
        blob_ref: String,
    },
    Media {
        blob_ref: String,
        mime: String,
        kind: MediaKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MediaKind {
    Image,
    Audio,
    Document,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntryInputView {
    pub kind: ContextEntryKindView,
    pub content_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_estimate: Option<TokenEstimateView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ContextEntryKindView {
    Message { role: ContextMessageRoleView },
    Instructions,
    VfsCatalog,
    EnvironmentCatalog,
    EnvironmentActive,
    SkillCatalog,
    SkillActivation { skill_id: SkillId },
    ToolCall { call_id: String, name: String },
    ToolResult { call_id: String, is_error: bool },
    ReasoningState,
    ProviderOpaque,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ContextMessageRoleView {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenEstimateView {
    pub tokens: u32,
    pub quality: TokenEstimateQualityView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TokenEstimateQualityView {
    Exact,
    ProviderCounted,
    Estimated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionItemView {
    UserMessage {
        id: ItemId,
        text: String,
    },
    AssistantMessage {
        id: ItemId,
        text: String,
    },
    ToolCall {
        id: ItemId,
        call_id: String,
        tool_name: String,
        arguments: Option<String>,
        status: ToolItemStatus,
    },
    ToolResult {
        id: ItemId,
        call_id: String,
        output: Option<String>,
        is_error: bool,
        status: ToolItemStatus,
    },
    SystemEvent {
        id: ItemId,
        text: String,
    },
    ProviderContext {
        id: ItemId,
        content_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_item_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_estimate: Option<TokenEstimateView>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<ProviderContextDisplayView>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToolItemStatus {
    Requested,
    Running,
    Succeeded,
    Failed,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", content = "params", rename_all = "camelCase")]
pub enum AgentNotification {
    #[serde(rename = "session/started")]
    SessionStarted { session: SessionView },
    #[serde(rename = "session/status/changed")]
    SessionStatusChanged {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        status: SessionStatus,
    },
    #[serde(rename = "session/event")]
    SessionEvent { event: SessionEventView },
    #[serde(rename = "run/started")]
    RunStarted {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        run: RunView,
    },
    #[serde(rename = "run/completed")]
    RunCompleted {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        run: RunView,
    },
    #[serde(rename = "item/completed")]
    ItemCompleted {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(rename = "runId")]
        run_id: RunId,
        item: SessionItemView,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "sessionId")]
        session_id: Option<SessionId>,
        message: String,
    },
}

impl AgentNotification {
    pub fn method(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => NOTIFY_SESSION_STARTED,
            Self::SessionStatusChanged { .. } => NOTIFY_SESSION_STATUS_CHANGED,
            Self::SessionEvent { .. } => NOTIFY_SESSION_EVENT,
            Self::RunStarted { .. } => NOTIFY_RUN_STARTED,
            Self::RunCompleted { .. } => NOTIFY_RUN_COMPLETED,
            Self::ItemCompleted { .. } => NOTIFY_ITEM_COMPLETED,
            Self::Error { .. } => NOTIFY_ERROR,
        }
    }

    pub fn into_json_rpc(self) -> Result<JsonRpcNotification, serde_json::Error> {
        let method = self.method().to_owned();
        let value = serde_json::to_value(self)?;
        let params = value.get("params").cloned();
        Ok(JsonRpcNotification { method, params })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentApiErrorKind {
    InvalidRequest,
    NotFound,
    Conflict,
    Rejected,
    UnsupportedAudioMime,
    AudioBlobTooLarge,
    AudioDurationTooLong,
    TranscoderUnavailable,
    TranscodeFailure,
    TranscriptionFailure,
    /// The session's agent workflow exists but failed during bootstrap
    /// (rehydration) and cannot serve runs. Distinct from `NotFound` (no
    /// workflow) so clients/bridges treat it as a session recovery problem
    /// rather than an ordinary "answer this message" failure.
    SessionBootstrapFailed,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Error)]
#[error("{kind:?}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct AgentApiError {
    pub kind: AgentApiErrorKind,
    pub message: String,
}

impl AgentApiError {
    pub fn new(kind: AgentApiErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::InvalidRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::NotFound, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::Conflict, message)
    }

    pub fn rejected(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::Rejected, message)
    }

    pub fn unsupported_audio_mime(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::UnsupportedAudioMime, message)
    }

    pub fn audio_blob_too_large(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::AudioBlobTooLarge, message)
    }

    pub fn audio_duration_too_long(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::AudioDurationTooLong, message)
    }

    pub fn transcoder_unavailable(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::TranscoderUnavailable, message)
    }

    pub fn transcode_failure(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::TranscodeFailure, message)
    }

    pub fn transcription_failure(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::TranscriptionFailure, message)
    }

    pub fn session_bootstrap_failed(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::SessionBootstrapFailed, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::Internal, message)
    }

    pub fn json_rpc_code(&self) -> i64 {
        match self.kind {
            AgentApiErrorKind::InvalidRequest
            | AgentApiErrorKind::UnsupportedAudioMime
            | AgentApiErrorKind::AudioBlobTooLarge
            | AgentApiErrorKind::AudioDurationTooLong
            | AgentApiErrorKind::TranscoderUnavailable => -32602,
            AgentApiErrorKind::NotFound => -32004,
            AgentApiErrorKind::Conflict => -32009,
            AgentApiErrorKind::Rejected
            | AgentApiErrorKind::TranscodeFailure
            | AgentApiErrorKind::TranscriptionFailure => -32010,
            AgentApiErrorKind::SessionBootstrapFailed => -32011,
            AgentApiErrorKind::Internal => -32603,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RequestId {
    Number(u64),
    String(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcRequest {
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcNotification {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcResponse {
    pub id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success<T: Serialize>(id: RequestId, result: T) -> Self {
        match serde_json::to_value(result) {
            Ok(result) => Self {
                id,
                result: Some(result),
                error: None,
            },
            Err(error) => Self::failure(id, JsonRpcError::internal(error.to_string())),
        }
    }

    pub fn failure(id: RequestId, error: JsonRpcError) -> Self {
        Self {
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<AgentApiError>,
}

impl JsonRpcError {
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }

    pub fn method_not_found(method: impl AsRef<str>) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {}", method.as_ref()),
            data: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }
}

impl From<AgentApiError> for JsonRpcError {
    fn from(error: AgentApiError) -> Self {
        let code = error.json_rpc_code();
        let message = error.message.clone();
        Self {
            code,
            message,
            data: Some(error),
        }
    }
}

/// Wire contract of one JSON-RPC method: its name, the Rust types of its
/// params and result, and a hook registering both schemas with a
/// [`schemars::SchemaGenerator`]. Produced by the same macro invocation that
/// generates [`dispatch_json_rpc`], so the manifest cannot drift from the
/// dispatcher.
pub struct MethodSpec {
    pub method: &'static str,
    pub params_type: &'static str,
    pub result_type: &'static str,
    pub register_schemas: fn(&mut schemars::SchemaGenerator) -> MethodSchemas,
}

pub struct MethodSchemas {
    pub params: schemars::Schema,
    pub result: schemars::Schema,
}

macro_rules! api_methods {
    ($($method_const:ident => $service_fn:ident($params:ty) -> $response:ty),+ $(,)?) => {
        pub async fn dispatch_json_rpc(
            service: &dyn AgentApiService,
            request: JsonRpcRequest,
        ) -> JsonRpcResponse {
            let id = request.id;
            match request.method.as_str() {
                $(
                    $method_const => match json_rpc_params::<$params>(request.params) {
                        Ok(params) => json_rpc_outcome(id, service.$service_fn(params).await),
                        Err(error) => JsonRpcResponse::failure(id, error),
                    },
                )+
                other => JsonRpcResponse::failure(id, JsonRpcError::method_not_found(other)),
            }
        }

        /// One entry per JSON-RPC method, in dispatch order. The JSON-RPC
        /// result envelope is `AgentApiOutcome<Response>`, which is what
        /// `result_type` and the registered result schema describe.
        pub fn method_manifest() -> Vec<MethodSpec> {
            vec![
                $(
                    MethodSpec {
                        method: $method_const,
                        params_type: stringify!($params),
                        result_type: concat!("AgentApiOutcome<", stringify!($response), ">"),
                        register_schemas: |generator| MethodSchemas {
                            params: generator.subschema_for::<$params>(),
                            result: generator.subschema_for::<AgentApiOutcome<$response>>(),
                        },
                    },
                )+
            ]
        }
    };
}

api_methods! {
    METHOD_INITIALIZE => initialize(InitializeParams) -> InitializeResponse,
    METHOD_SESSION_START => start_session(SessionStartParams) -> SessionStartResponse,
    METHOD_SESSION_UPDATE => update_session(SessionUpdateParams) -> SessionUpdateResponse,
    METHOD_SESSION_TOOLS_UPDATE => update_session_tools(SessionToolsUpdateParams) -> SessionToolsUpdateResponse,
    METHOD_SESSION_READ => read_session(SessionReadParams) -> SessionReadResponse,
    METHOD_SESSION_EVENTS_READ => read_session_events(SessionEventsReadParams) -> SessionEventsReadResponse,
    METHOD_SESSION_CLOSE => close_session(SessionCloseParams) -> SessionCloseResponse,
    METHOD_CONTEXT_COMPACT => compact_context(ContextCompactParams) -> ContextCompactResponse,
    METHOD_CONTEXT_APPEND => append_context(ContextAppendParams) -> ContextAppendResponse,
    METHOD_OUTBOX_READ => read_outbox(OutboxReadParams) -> OutboxReadResponse,
    METHOD_OUTBOX_ACK => ack_outbox(OutboxAckParams) -> OutboxAckResponse,
    METHOD_RUN_START => start_run(RunStartParams) -> RunStartResponse,
    METHOD_RUN_CANCEL => cancel_run(RunCancelParams) -> RunCancelResponse,
    METHOD_PROMPTS_ACTIVE => active_prompts(PromptsActiveParams) -> PromptsActiveResponse,
    METHOD_SKILLS_LIST => list_skills(SkillListParams) -> SkillListResponse,
    METHOD_SKILLS_ACTIVE => active_skills(SkillActiveParams) -> SkillActiveResponse,
    METHOD_SKILLS_ACTIVATE => activate_skill(SkillActivateParams) -> SkillActivateResponse,
    METHOD_SKILLS_DEACTIVATE => deactivate_skill(SkillDeactivateParams) -> SkillDeactivateResponse,
    METHOD_SESSION_ENVIRONMENTS_LIST => list_session_environments(SessionEnvironmentListParams) -> SessionEnvironmentListResponse,
    METHOD_SESSION_ENVIRONMENTS_READ => read_session_environment(SessionEnvironmentReadParams) -> SessionEnvironmentReadResponse,
    METHOD_SESSION_ENVIRONMENTS_CREATE => create_session_environment(SessionEnvironmentCreateParams) -> SessionEnvironmentCreateResponse,
    METHOD_SESSION_ENVIRONMENTS_ATTACH => attach_session_environment(SessionEnvironmentAttachParams) -> SessionEnvironmentAttachResponse,
    METHOD_SESSION_ENVIRONMENTS_ACTIVATE => activate_session_environment(SessionEnvironmentActivateParams) -> SessionEnvironmentActivateResponse,
    METHOD_SESSION_ENVIRONMENTS_DEACTIVATE => deactivate_session_environment(SessionEnvironmentDeactivateParams) -> SessionEnvironmentDeactivateResponse,
    METHOD_SESSION_ENVIRONMENTS_CLOSE => close_session_environment(SessionEnvironmentCloseParams) -> SessionEnvironmentCloseResponse,
    METHOD_ENVIRONMENT_PROVIDERS_REGISTER => register_environment_provider(EnvironmentProviderRegisterParams) -> EnvironmentProviderRegisterResponse,
    METHOD_ENVIRONMENT_PROVIDERS_HEARTBEAT => heartbeat_environment_provider(EnvironmentProviderHeartbeatParams) -> EnvironmentProviderHeartbeatResponse,
    METHOD_ENVIRONMENT_PROVIDERS_UNREGISTER => unregister_environment_provider(EnvironmentProviderUnregisterParams) -> EnvironmentProviderUnregisterResponse,
    METHOD_BLOB_PUT => put_blob(BlobPutParams) -> BlobPutResponse,
    METHOD_BLOB_PUT_MANY => put_blobs(BlobPutManyParams) -> BlobPutManyResponse,
    METHOD_BLOB_GET => get_blob(BlobGetParams) -> BlobGetResponse,
    METHOD_BLOB_HAS_MANY => has_blobs(BlobHasManyParams) -> BlobHasManyResponse,
    METHOD_VFS_SNAPSHOT_COMMIT => commit_vfs_snapshot(VfsSnapshotCommitParams) -> VfsSnapshotCommitResponse,
    METHOD_VFS_SNAPSHOT_READ => read_vfs_snapshot(VfsSnapshotReadParams) -> VfsSnapshotReadResponse,
    METHOD_VFS_WORKSPACE_CREATE => create_vfs_workspace(VfsWorkspaceCreateParams) -> VfsWorkspaceCreateResponse,
    METHOD_VFS_WORKSPACE_READ => read_vfs_workspace(VfsWorkspaceReadParams) -> VfsWorkspaceReadResponse,
    METHOD_VFS_WORKSPACE_UPDATE => update_vfs_workspace(VfsWorkspaceUpdateParams) -> VfsWorkspaceUpdateResponse,
    METHOD_VFS_WORKSPACE_DELETE => delete_vfs_workspace(VfsWorkspaceDeleteParams) -> VfsWorkspaceDeleteResponse,
    METHOD_VFS_MOUNT_PUT => put_vfs_mount(VfsMountPutParams) -> VfsMountPutResponse,
    METHOD_VFS_MOUNT_DELETE => delete_vfs_mount(VfsMountDeleteParams) -> VfsMountDeleteResponse,
    METHOD_VFS_MOUNT_LIST => list_vfs_mounts(VfsMountListParams) -> VfsMountListResponse,
    METHOD_MCP_SERVERS_CREATE => create_mcp_server(McpServerCreateParams) -> McpServerCreateResponse,
    METHOD_MCP_SERVERS_LIST => list_mcp_servers(McpServerListParams) -> McpServerListResponse,
    METHOD_MCP_SERVERS_READ => read_mcp_server(McpServerReadParams) -> McpServerReadResponse,
    METHOD_MCP_SERVERS_DELETE => delete_mcp_server(McpServerDeleteParams) -> McpServerDeleteResponse,
    METHOD_SESSION_MCP_LINK => link_session_mcp(SessionMcpLinkParams) -> SessionMcpLinkResponse,
    METHOD_SESSION_MCP_UNLINK => unlink_session_mcp(SessionMcpUnlinkParams) -> SessionMcpUnlinkResponse,
    METHOD_SESSION_MCP_LIST => list_session_mcp(SessionMcpListParams) -> SessionMcpListResponse,
    METHOD_AUTH_GRANTS_IMPORT => import_auth_grant(AuthGrantImportParams) -> AuthGrantImportResponse,
    METHOD_AUTH_GRANTS_LIST => list_auth_grants(AuthGrantListParams) -> AuthGrantListResponse,
    METHOD_AUTH_GRANTS_READ => read_auth_grant(AuthGrantReadParams) -> AuthGrantReadResponse,
    METHOD_AUTH_GRANTS_REVOKE => revoke_auth_grant(AuthGrantRevokeParams) -> AuthGrantRevokeResponse,
    METHOD_AUTH_CLIENTS_CREATE => create_auth_client(AuthClientCreateParams) -> AuthClientCreateResponse,
    METHOD_AUTH_CLIENTS_LIST => list_auth_clients(AuthClientListParams) -> AuthClientListResponse,
    METHOD_AUTH_CLIENTS_READ => read_auth_client(AuthClientReadParams) -> AuthClientReadResponse,
    METHOD_AUTH_CLIENTS_DELETE => delete_auth_client(AuthClientDeleteParams) -> AuthClientDeleteResponse,
    METHOD_AUTH_FLOWS_START => start_auth_flow(AuthFlowStartParams) -> AuthFlowStartResponse,
    METHOD_AUTH_FLOWS_STATUS => read_auth_flow_status(AuthFlowStatusParams) -> AuthFlowStatusResponse,
    METHOD_AUTH_PROVIDERS_CREATE => create_auth_provider(AuthProviderCreateParams) -> AuthProviderCreateResponse,
    METHOD_AUTH_PROVIDERS_LIST => list_auth_providers(AuthProviderListParams) -> AuthProviderListResponse,
    METHOD_AUTH_PROVIDERS_READ => read_auth_provider(AuthProviderReadParams) -> AuthProviderReadResponse,
    METHOD_AUTH_PROVIDERS_DELETE => delete_auth_provider(AuthProviderDeleteParams) -> AuthProviderDeleteResponse,
    METHOD_AUTH_GITHUB_INSTALLATIONS_LIST => list_github_installations(AuthGitHubInstallationListParams) -> AuthGitHubInstallationListResponse,
    METHOD_AUTH_GITHUB_INSTALLATIONS_GRANT => grant_github_installation(AuthGitHubInstallationGrantParams) -> AuthGitHubInstallationGrantResponse,
}

/// JSON-RPC notification methods the server can emit, with payloads described
/// by the [`AgentNotification`] schema.
pub const NOTIFICATION_METHODS: &[&str] = &[
    NOTIFY_SESSION_STARTED,
    NOTIFY_SESSION_STATUS_CHANGED,
    NOTIFY_SESSION_EVENT,
    NOTIFY_RUN_STARTED,
    NOTIFY_RUN_COMPLETED,
    NOTIFY_ITEM_COMPLETED,
    NOTIFY_ERROR,
];

fn json_rpc_params<T>(params: Option<Value>) -> Result<T, JsonRpcError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(params.unwrap_or_else(|| Value::Object(Default::default())))
        .map_err(|error| JsonRpcError::invalid_params(error.to_string()))
}

fn json_rpc_outcome<T>(
    id: RequestId,
    outcome: Result<AgentApiOutcome<T>, AgentApiError>,
) -> JsonRpcResponse
where
    T: Serialize,
{
    match outcome {
        Ok(outcome) => JsonRpcResponse::success(id, outcome),
        Err(error) => JsonRpcResponse::failure(id, error.into()),
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;

    #[test]
    fn notification_serializes_as_json_rpc_lite_shape() {
        let notification = AgentNotification::RunCompleted {
            session_id: "session_1".to_owned(),
            run: RunView {
                id: "run_1".to_owned(),
                status: RunStatus::Completed,
                input: vec![InputItem::Text {
                    text: "hello".to_owned(),
                }],
                items: Vec::new(),
                tool_batches: Vec::new(),
            },
        };

        let value = serde_json::to_value(notification).expect("serialize notification");

        assert_eq!(
            value,
            json!({
                "method": "run/completed",
                "params": {
                    "sessionId": "session_1",
                    "run": {
                        "id": "run_1",
                        "status": "completed",
                        "input": [{ "type": "text", "text": "hello" }],
                        "items": []
                    }
                }
            })
        );
    }

    #[test]
    fn auth_grant_import_params_redact_token_in_debug_output() {
        let params: AuthGrantImportParams = serde_json::from_value(json!({
            "grantId": "authgrant_1",
            "token": "super-secret-token",
            "audience": "https://crm.example.com/mcp"
        }))
        .expect("deserialize import params");

        let debug = format!("{params:?}");

        assert!(!debug.contains("super-secret-token"), "{debug}");
        assert!(debug.contains("<redacted>"));
        assert_eq!(params.token, "super-secret-token");
    }

    #[test]
    fn auth_client_create_params_redact_client_secret_in_debug_output() {
        let params: AuthClientCreateParams = serde_json::from_value(json!({
            "providerKind": "customOAuth",
            "authorizationEndpoint": "https://as.example.com/authorize",
            "tokenEndpoint": "https://as.example.com/token",
            "remoteClientId": "client-1",
            "clientSecret": "super-secret-client-secret"
        }))
        .expect("deserialize client create params");

        let debug = format!("{params:?}");

        assert!(!debug.contains("super-secret-client-secret"), "{debug}");
        assert!(debug.contains("<redacted>"));
        assert_eq!(
            params.client_secret.as_deref(),
            Some("super-secret-client-secret")
        );
    }

    #[test]
    fn auth_provider_create_params_redact_credential_in_debug_output() {
        let params: AuthProviderCreateParams = serde_json::from_value(json!({
            "providerId": "lightspeed-github",
            "config": {"type": "githubApp", "appId": "12345"},
            "credential": "-----BEGIN RSA PRIVATE KEY-----\nsuper-secret-key"
        }))
        .expect("deserialize provider create params");

        let debug = format!("{params:?}");

        assert!(!debug.contains("super-secret-key"), "{debug}");
        assert!(debug.contains("<redacted>"));
        assert!(
            params
                .credential
                .as_deref()
                .unwrap()
                .contains("super-secret-key")
        );
    }

    #[test]
    fn request_ids_accept_number_or_string() {
        let numeric: JsonRpcRequest = serde_json::from_value(json!({
            "id": 7,
            "method": "session/start"
        }))
        .expect("numeric id");
        let string: JsonRpcRequest = serde_json::from_value(json!({
            "id": "req_7",
            "method": "session/start"
        }))
        .expect("string id");

        assert_eq!(numeric.id, RequestId::Number(7));
        assert_eq!(string.id, RequestId::String("req_7".to_owned()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_calls_api_service() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_INITIALIZE.to_owned(),
                params: Some(json!({})),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["serverInfo"]["name"],
            json!("test-service")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_rejects_unknown_methods() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::String("req_1".to_owned()),
                method: "missing/method".to_owned(),
                params: None,
            },
        )
        .await;

        assert_eq!(response.error.expect("error").code, -32601);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_session_close() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SESSION_CLOSE.to_owned(),
                params: Some(json!({ "sessionId": "session_1" })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["session"]["status"],
            json!("closed")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_session_update() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SESSION_UPDATE.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "expectedConfigRevision": 0,
                    "patch": {
                        "instructions": {
                            "op": "set",
                            "value": {
                                "type": "text",
                                "text": "answer tersely"
                            }
                        }
                    }
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["session"]["id"],
            json!("session_1")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_session_tools_update() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SESSION_TOOLS_UPDATE.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "expectedToolsRevision": 4,
                    "update": {
                        "type": "patch",
                        "upsert": [],
                        "remove": []
                    }
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["session"]["activeTools"]["revision"],
            json!(5)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_context_compact() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_CONTEXT_COMPACT.to_owned(),
                params: Some(json!({ "sessionId": "session_1" })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["session"]["id"],
            json!("session_1")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_context_append() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_CONTEXT_APPEND.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "entries": [
                        {
                            "key": "channel.room.batch-1",
                            "item": { "type": "text", "text": "Alice: hello" }
                        }
                    ]
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["appliedKeys"],
            json!(["channel.room.batch-1"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_outbox_read_and_ack() {
        let read = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_OUTBOX_READ.to_owned(),
                params: Some(json!({ "after": 7, "waitMs": 100 })),
            },
        )
        .await;
        assert!(read.error.is_none());
        let read = read.result.expect("result");
        assert_eq!(read["result"]["nextAfter"], json!(8));
        assert_eq!(
            read["result"]["entries"][0]["payload"]["type"],
            json!("send")
        );

        let ack = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(2),
                method: METHOD_OUTBOX_ACK.to_owned(),
                params: Some(json!({
                    "outboxId": "outbox_1",
                    "result": { "type": "delivered", "channelMessageId": "42" }
                })),
            },
        )
        .await;
        assert!(ack.error.is_none());
        assert_eq!(
            ack.result.expect("result")["result"]["status"],
            json!("delivered")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_run_cancel() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_RUN_CANCEL.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "runId": "run_1"
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["run"]["status"],
            json!("cancelled")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_prompts_active() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_PROMPTS_ACTIVE.to_owned(),
                params: Some(json!({ "sessionId": "session_1" })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["report"]["total_chars"],
            json!(42)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_skills_list() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SKILLS_LIST.to_owned(),
                params: Some(json!({ "sessionId": "session_1" })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["skills"][0]["active"],
            json!(true)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_skills_active() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SKILLS_ACTIVE.to_owned(),
                params: Some(json!({ "sessionId": "session_1" })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["activations"][0]["source"]["type"],
            json!("directContext")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_skills_activate() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SKILLS_ACTIVATE.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "skillId": "skill:one",
                    "scope": "session"
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["activation"]["scope"],
            json!("session")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_skills_deactivate() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SKILLS_DEACTIVATE.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "skillId": "skill:one"
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["skillId"],
            json!("skill:one")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_session_environments_list() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SESSION_ENVIRONMENTS_LIST.to_owned(),
                params: Some(json!({ "sessionId": "session_1" })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["environments"][0]["active"],
            json!(true)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_session_environments_read() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SESSION_ENVIRONMENTS_READ.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "envId": "test"
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["environment"]["envId"],
            json!("test")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_session_environments_create() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SESSION_ENVIRONMENTS_CREATE.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "envId": "test",
                    "providerId": "sandbox-pool",
                    "request": {
                        "type": "sandbox",
                        "spec": {
                            "image": "ubuntu:latest",
                            "cwd": "/workspace"
                        }
                    },
                    "activate": true
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["environment"]["envId"],
            json!("test")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_session_environments_attach() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SESSION_ENVIRONMENTS_ATTACH.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "envId": "test",
                    "providerId": "bridge-local",
                    "request": {
                        "type": "target",
                        "targetId": "local-host"
                    }
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["environment"]["kind"],
            json!("attachedHost")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_session_environments_activate() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SESSION_ENVIRONMENTS_ACTIVATE.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "envId": "test"
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["activeEnvId"],
            json!("test")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_session_environments_deactivate() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SESSION_ENVIRONMENTS_DEACTIVATE.to_owned(),
                params: Some(json!({ "sessionId": "session_1" })),
            },
        )
        .await;

        assert!(response.error.is_none());
        let result = response.result.expect("result");
        assert!(result["result"]["activeEnvId"].is_null());
        assert_eq!(result["result"]["environments"][0]["active"], json!(false));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_session_environments_close() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SESSION_ENVIRONMENTS_CLOSE.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "envId": "test",
                    "force": true
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["environment"]["status"],
            json!("detached")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_environment_provider_register() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_ENVIRONMENT_PROVIDERS_REGISTER.to_owned(),
                params: Some(json!({
                    "providerId": "bridge-local",
                    "providerKind": "bridge",
                    "controllerConnection": {
                        "endpoint": "ws://127.0.0.1:9000/controller",
                        "transport": { "type": "webSocket" }
                    },
                    "capabilities": {
                        "listTargets": true,
                        "attachTarget": true,
                        "getTarget": true
                    },
                    "implementation": {
                        "name": "test-bridge",
                        "version": "1.0.0"
                    },
                    "leaseTtlMs": 30000
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["provider"]["providerId"],
            json!("bridge-local")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_environment_provider_heartbeat() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_ENVIRONMENT_PROVIDERS_HEARTBEAT.to_owned(),
                params: Some(json!({
                    "providerId": "bridge-local",
                    "observedTargets": [{
                        "targetId": "local-host",
                        "status": "ready",
                        "scope": { "type": "default" },
                        "capabilities": {
                            "filesystemRead": true,
                            "filesystemWrite": true,
                            "processStart": true,
                            "processStdin": true
                        },
                        "defaultCwd": "/workspace"
                    }]
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["targets"][0]["targetId"],
            json!("local-host")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_environment_provider_unregister() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_ENVIRONMENT_PROVIDERS_UNREGISTER.to_owned(),
                params: Some(json!({ "providerId": "bridge-local" })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["provider"]["status"],
            json!("offline")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_mcp_server_create() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_MCP_SERVERS_CREATE.to_owned(),
                params: Some(json!({
                    "serverId": "echo",
                    "serverUrl": "https://echo.example.com/mcp",
                    "defaultServerLabel": "echo"
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["server"]["serverId"],
            json!("echo")
        );
    }

    #[test]
    fn mcp_server_create_params_default_approval_is_never() {
        let params: McpServerCreateParams = serde_json::from_value(json!({
            "serverId": "echo",
            "serverUrl": "https://echo.example.com/mcp",
            "defaultServerLabel": "echo"
        }))
        .expect("params");

        assert_eq!(params.approval_default, RemoteMcpApprovalPolicy::Never);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_session_mcp_link() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SESSION_MCP_LINK.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "serverId": "echo",
                    "toolId": "mcp_echo"
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["link"]["toolId"],
            json!("mcp_echo")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_session_mcp_unlink() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_SESSION_MCP_UNLINK.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "toolId": "mcp_echo"
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["toolId"],
            json!("mcp_echo")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_run_start_with_config() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_RUN_START.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "input": [{ "type": "text", "text": "hello" }],
                    "config": {
                        "model": {
                            "providerId": "openai",
                            "apiKind": "openai:responses",
                            "model": "gpt-5.5"
                        },
                        "generation": {
                            "maxOutputTokens": 1024,
                            "reasoningEffort": "high"
                        }
                    }
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["run"]["status"],
            json!("running")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_blob_put_many() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_BLOB_PUT_MANY.to_owned(),
                params: Some(json!({
                    "blobs": [
                        { "bytesBase64": "aGVsbG8=" },
                        { "bytesBase64": "d29ybGQ=" }
                    ]
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["blobs"][1]["bytes"],
            json!(8)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_vfs_snapshot_commit() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_VFS_SNAPSHOT_COMMIT.to_owned(),
                params: Some(json!({
                    "manifest": {
                        "schema_version": "lightspeed.vfs.snapshot.v1",
                        "root": { "entries": {} },
                        "totals": { "files": 0, "bytes": 0 }
                    }
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["snapshotRef"],
            json!(format!("sha256:{}", "2".repeat(64)))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_vfs_workspace_create() {
        let snapshot_ref = format!("sha256:{}", "2".repeat(64));
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_VFS_WORKSPACE_CREATE.to_owned(),
                params: Some(json!({
                    "workspaceId": "workspace_1",
                    "snapshotRef": snapshot_ref
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["workspace"]["workspaceId"],
            json!("workspace_1")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_vfs_workspace_read() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_VFS_WORKSPACE_READ.to_owned(),
                params: Some(json!({ "workspaceId": "workspace_1" })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["workspace"]["revision"],
            json!(4)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_vfs_workspace_update() {
        let snapshot_ref = format!("sha256:{}", "4".repeat(64));
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_VFS_WORKSPACE_UPDATE.to_owned(),
                params: Some(json!({
                    "workspaceId": "workspace_1",
                    "expectedRevision": 4,
                    "snapshotRef": snapshot_ref
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["workspace"]["revision"],
            json!(5)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_vfs_workspace_update_without_expected_revision() {
        let snapshot_ref = format!("sha256:{}", "4".repeat(64));
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_VFS_WORKSPACE_UPDATE.to_owned(),
                params: Some(json!({
                    "workspaceId": "workspace_1",
                    "snapshotRef": snapshot_ref
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["workspace"]["revision"],
            json!(5)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_vfs_workspace_delete() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_VFS_WORKSPACE_DELETE.to_owned(),
                params: Some(json!({ "workspaceId": "workspace_1" })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["workspace"]["workspaceId"],
            json!("workspace_1")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_vfs_mount_put() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_VFS_MOUNT_PUT.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "mountPath": "/workspace",
                    "source": { "type": "workspace", "workspaceId": "workspace_1" },
                    "access": "readWrite"
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["mount"]["source"]["workspaceId"],
            json!("workspace_1")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_json_rpc_routes_vfs_mount_delete() {
        let response = dispatch_json_rpc(
            &TestService,
            JsonRpcRequest {
                id: RequestId::Number(1),
                method: METHOD_VFS_MOUNT_DELETE.to_owned(),
                params: Some(json!({
                    "sessionId": "session_1",
                    "mountPath": "/workspace"
                })),
            },
        )
        .await;

        assert!(response.error.is_none());
        assert_eq!(
            response.result.expect("result")["result"]["mountPath"],
            json!("/workspace")
        );
    }

    #[test]
    fn session_event_serializes_with_cursor_and_kind() {
        let event = SessionEventView {
            cursor: EventCursor { seq: 3 },
            session_id: "session_1".to_owned(),
            observed_at_ms: 12,
            joins: EventJoinsView {
                run_id: Some("run_1".to_owned()),
                ..EventJoinsView::default()
            },
            kind: SessionEventKindView::RunCompleted {
                run_id: "run_1".to_owned(),
                output_ref: Some("sha256:abc".to_owned()),
            },
        };

        let value = serde_json::to_value(AgentNotification::SessionEvent { event })
            .expect("serialize event notification");

        assert_eq!(
            value,
            json!({
                "method": "session/event",
                "params": {
                    "event": {
                        "cursor": { "seq": 3 },
                        "sessionId": "session_1",
                        "observedAtMs": 12,
                        "joins": { "runId": "run_1" },
                        "kind": {
                            "type": "runCompleted",
                            "runId": "run_1",
                            "outputRef": "sha256:abc"
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn tool_batch_started_event_can_inline_tool_arguments() {
        let event = SessionEventView {
            cursor: EventCursor { seq: 4 },
            session_id: "session_1".to_owned(),
            observed_at_ms: 12,
            joins: EventJoinsView {
                run_id: Some("run_1".to_owned()),
                tool_batch_id: Some("tool_batch_1".to_owned()),
                ..EventJoinsView::default()
            },
            kind: SessionEventKindView::ToolBatchStarted {
                run_id: "run_1".to_owned(),
                turn_id: "turn_1".to_owned(),
                batch_id: "tool_batch_1".to_owned(),
                calls: vec![ToolCallEventView {
                    call_id: "call_1".to_owned(),
                    tool_name: "read_file".to_owned(),
                    arguments_ref: "sha256:args".to_owned(),
                    arguments: Some(r#"{"path":"README.md"}"#.to_owned()),
                    display: Some(ToolCallDisplayView {
                        group: ToolCallDisplayGroup::Explore,
                        verb: "Read".to_owned(),
                        target: Some("README.md".to_owned()),
                        detail: None,
                    }),
                }],
            },
        };

        let value = serde_json::to_value(event).expect("serialize event");

        assert_eq!(
            value["kind"]["calls"][0],
            json!({
                "callId": "call_1",
                "toolName": "read_file",
                "argumentsRef": "sha256:args",
                "arguments": "{\"path\":\"README.md\"}",
                "display": {
                    "group": "explore",
                    "verb": "Read",
                    "target": "README.md"
                }
            })
        );
    }

    #[test]
    fn provider_context_item_serializes_debug_metadata() {
        let item = SessionItemView::ProviderContext {
            id: "item_42".to_owned(),
            content_ref: "sha256:compact".to_owned(),
            media_type: Some("application/json".to_owned()),
            preview: Some("OpenAI Responses compaction item".to_owned()),
            provider_kind: Some("openai.responses.compaction".to_owned()),
            provider_item_id: Some("item_compaction_1".to_owned()),
            token_estimate: Some(TokenEstimateView {
                tokens: 123,
                quality: TokenEstimateQualityView::ProviderCounted,
            }),
            display: None,
        };

        let value = serde_json::to_value(item).expect("serialize provider context item");

        assert_eq!(
            value,
            json!({
                "type": "providerContext",
                "id": "item_42",
                "contentRef": "sha256:compact",
                "mediaType": "application/json",
                "preview": "OpenAI Responses compaction item",
                "providerKind": "openai.responses.compaction",
                "providerItemId": "item_compaction_1",
                "tokenEstimate": {
                    "tokens": 123,
                    "quality": "providerCounted"
                }
            })
        );
    }

    #[test]
    fn provider_context_item_serializes_mcp_display() {
        let item = SessionItemView::ProviderContext {
            id: "item_43".to_owned(),
            content_ref: "sha256:mcp".to_owned(),
            media_type: Some("application/json".to_owned()),
            preview: Some("OpenAI Responses MCP tool call: echo.echo".to_owned()),
            provider_kind: Some("openai.responses.mcp_call".to_owned()),
            provider_item_id: Some("mcp_1".to_owned()),
            token_estimate: None,
            display: Some(ProviderContextDisplayView {
                summary: ToolCallDisplayView {
                    group: ToolCallDisplayGroup::Other,
                    verb: "MCP".to_owned(),
                    target: Some("echo.echo".to_owned()),
                    detail: None,
                },
                tool_name: "echo.echo".to_owned(),
                status: ToolItemStatus::Succeeded,
                is_error: false,
                arguments: Some(r#"{"data":"simba"}"#.to_owned()),
                output: Some("Echoing your input: simba".to_owned()),
                error: None,
            }),
        };

        let value = serde_json::to_value(item).expect("serialize mcp provider context item");

        assert_eq!(
            value,
            json!({
                "type": "providerContext",
                "id": "item_43",
                "contentRef": "sha256:mcp",
                "mediaType": "application/json",
                "preview": "OpenAI Responses MCP tool call: echo.echo",
                "providerKind": "openai.responses.mcp_call",
                "providerItemId": "mcp_1",
                "display": {
                    "summary": {
                        "group": "other",
                        "verb": "MCP",
                        "target": "echo.echo"
                    },
                    "toolName": "echo.echo",
                    "status": "succeeded",
                    "isError": false,
                    "arguments": "{\"data\":\"simba\"}",
                    "output": "Echoing your input: simba"
                }
            })
        );
    }

    #[test]
    fn run_view_can_expose_tool_batches() {
        let run = RunView {
            id: "run_1".to_owned(),
            status: RunStatus::Running,
            input: Vec::new(),
            items: Vec::new(),
            tool_batches: vec![ToolBatchView {
                id: "tool_batch_1".to_owned(),
                turn_id: "turn_1".to_owned(),
                status: ToolItemStatus::Succeeded,
                calls: vec![ToolCallView {
                    call_id: "call_1".to_owned(),
                    tool_name: "read_file".to_owned(),
                    arguments_ref: "sha256:args".to_owned(),
                    arguments: Some(r#"{"path":"README.md"}"#.to_owned()),
                    output: Some("ok".to_owned()),
                    is_error: false,
                    status: ToolItemStatus::Succeeded,
                    effects: Vec::new(),
                    display: Some(ToolCallDisplayView {
                        group: ToolCallDisplayGroup::Explore,
                        verb: "Read".to_owned(),
                        target: Some("README.md".to_owned()),
                        detail: None,
                    }),
                }],
            }],
        };

        let value = serde_json::to_value(run).expect("serialize run");

        assert_eq!(
            value["toolBatches"][0],
            json!({
                "id": "tool_batch_1",
                "turnId": "turn_1",
                "status": "succeeded",
                "calls": [{
                    "callId": "call_1",
                    "toolName": "read_file",
                    "argumentsRef": "sha256:args",
                    "arguments": "{\"path\":\"README.md\"}",
                    "output": "ok",
                    "isError": false,
                    "status": "succeeded",
                    "display": {
                        "group": "explore",
                        "verb": "Read",
                        "target": "README.md"
                    }
                }]
            })
        );
    }

    #[test]
    fn session_status_serializes_as_string_enum() {
        assert_eq!(
            serde_json::to_value(SessionStatus::Idle).expect("serialize status"),
            json!("idle")
        );
    }

    #[test]
    fn run_lifecycle_statuses_keep_cancelling_distinct() {
        assert_eq!(
            serde_json::to_value(RunStatus::Cancelling).expect("serialize status"),
            json!("cancelling")
        );
    }

    #[test]
    fn tool_call_status_can_represent_requested_calls() {
        assert_eq!(
            serde_json::to_value(ToolItemStatus::Requested).expect("serialize status"),
            json!("requested")
        );
    }

    #[test]
    fn session_id_validation_matches_public_api_shape() {
        assert_eq!(validate_session_id("session-1"), Ok(()));
        assert_eq!(validate_session_id("session_1.test:dev"), Ok(()));
        assert_eq!(validate_session_id(""), Err(SessionIdError::Empty));
        assert_eq!(
            validate_session_id("-session"),
            Err(SessionIdError::InvalidStart)
        );
        assert_eq!(
            validate_session_id("session/name"),
            Err(SessionIdError::InvalidCharacter { index: 7, ch: '/' })
        );
        assert_eq!(
            validate_session_id("session name"),
            Err(SessionIdError::InvalidCharacter { index: 7, ch: ' ' })
        );
    }

    struct TestService;

    #[async_trait]
    impl AgentApiService for TestService {
        async fn initialize(
            &self,
            _params: InitializeParams,
        ) -> Result<AgentApiOutcome<InitializeResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(InitializeResponse {
                protocol_version: PROTOCOL_VERSION.to_owned(),
                server_info: ServerInfo {
                    name: "test-service".to_owned(),
                    version: "0".to_owned(),
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
            _params: SessionStartParams,
        ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
            Err(AgentApiError::internal("not implemented"))
        }

        async fn update_session(
            &self,
            params: SessionUpdateParams,
        ) -> Result<AgentApiOutcome<SessionUpdateResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(SessionUpdateResponse {
                session: test_session(params.session_id, SessionStatus::Idle),
            }))
        }

        async fn update_session_tools(
            &self,
            params: SessionToolsUpdateParams,
        ) -> Result<AgentApiOutcome<SessionToolsUpdateResponse>, AgentApiError> {
            let mut session = test_session(params.session_id, SessionStatus::Idle);
            session.active_tools.revision = params.expected_tools_revision.unwrap_or(0) + 1;
            Ok(AgentApiOutcome::new(SessionToolsUpdateResponse { session }))
        }

        async fn read_session(
            &self,
            _params: SessionReadParams,
        ) -> Result<AgentApiOutcome<SessionReadResponse>, AgentApiError> {
            Err(AgentApiError::internal("not implemented"))
        }

        async fn read_session_events(
            &self,
            _params: SessionEventsReadParams,
        ) -> Result<AgentApiOutcome<SessionEventsReadResponse>, AgentApiError> {
            Err(AgentApiError::internal("not implemented"))
        }

        async fn close_session(
            &self,
            params: SessionCloseParams,
        ) -> Result<AgentApiOutcome<SessionCloseResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(SessionCloseResponse {
                session: test_session(params.session_id, SessionStatus::Closed),
            }))
        }

        async fn compact_context(
            &self,
            params: ContextCompactParams,
        ) -> Result<AgentApiOutcome<ContextCompactResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(ContextCompactResponse {
                session: test_session(params.session_id, SessionStatus::Idle),
            }))
        }

        async fn append_context(
            &self,
            params: ContextAppendParams,
        ) -> Result<AgentApiOutcome<ContextAppendResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(ContextAppendResponse {
                context_revision: 1,
                applied_keys: params
                    .entries
                    .iter()
                    .map(|entry| entry.key.clone())
                    .collect(),
                unchanged_keys: Vec::new(),
            }))
        }

        async fn read_outbox(
            &self,
            params: OutboxReadParams,
        ) -> Result<AgentApiOutcome<OutboxReadResponse>, AgentApiError> {
            let after = params.after.unwrap_or(0);
            Ok(AgentApiOutcome::new(OutboxReadResponse {
                entries: vec![OutboundMessageView {
                    seq: after + 1,
                    outbox_id: "outbox_1".to_owned(),
                    session_id: "session_1".to_owned(),
                    run_id: Some("run_1".to_owned()),
                    origin: OutboundOriginView::ToolCall,
                    payload: OutboundPayloadView::Send {
                        text: "hello".to_owned(),
                        reply_to: None,
                    },
                    attempts: 0,
                    created_at_ms: 1,
                }],
                next_after: after + 1,
            }))
        }

        async fn ack_outbox(
            &self,
            params: OutboxAckParams,
        ) -> Result<AgentApiOutcome<OutboxAckResponse>, AgentApiError> {
            let status = match params.result {
                OutboundAckInput::Delivered { .. } => OutboundStatusView::Delivered,
                OutboundAckInput::Failed { .. } => OutboundStatusView::Failed,
            };
            Ok(AgentApiOutcome::new(OutboxAckResponse {
                outbox_id: params.outbox_id,
                status,
                attempts: 1,
            }))
        }

        async fn start_run(
            &self,
            params: RunStartParams,
        ) -> Result<AgentApiOutcome<RunStartResponse>, AgentApiError> {
            let config = params.config.expect("run config");
            assert_eq!(params.session_id, "session_1");
            let generation = config.generation.expect("generation");
            assert_eq!(generation.max_output_tokens, Some(1024));
            assert_eq!(generation.reasoning_effort, Some(ReasoningEffort::High));
            assert_eq!(config.model.expect("model").model, "gpt-5.5");
            Ok(AgentApiOutcome::new(RunStartResponse {
                run: test_run("run_1".to_owned(), RunStatus::Running),
            }))
        }

        async fn cancel_run(
            &self,
            params: RunCancelParams,
        ) -> Result<AgentApiOutcome<RunCancelResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(RunCancelResponse {
                run: test_run(params.run_id, RunStatus::Cancelled),
            }))
        }

        async fn active_prompts(
            &self,
            _params: PromptsActiveParams,
        ) -> Result<AgentApiOutcome<PromptsActiveResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(PromptsActiveResponse {
                instructions: vec![PromptInstructionView {
                    key: "instructions.100.prompts.0000.project".to_owned(),
                    instructions_ref: format!("sha256:{}", "4".repeat(64)),
                    media_type: Some("text/markdown".to_owned()),
                    preview: Some("prompt instructions: instructions.md".to_owned()),
                }],
                report_ref: Some(format!("sha256:{}", "5".repeat(64))),
                report: Some(json!({
                    "schema_version": "lightspeed.prompts.instructions.report.v1",
                    "total_chars": 42
                })),
            }))
        }

        async fn list_skills(
            &self,
            _params: SkillListParams,
        ) -> Result<AgentApiOutcome<SkillListResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(SkillListResponse {
                catalog_ref: Some(format!("sha256:{}", "5".repeat(64))),
                skills: vec![SkillListItem {
                    skill_id: "skill:one".to_owned(),
                    name: "one".to_owned(),
                    description: "Use when testing skills.".to_owned(),
                    short_description: Some("test skill".to_owned()),
                    enabled: true,
                    active: true,
                }],
            }))
        }

        async fn active_skills(
            &self,
            _params: SkillActiveParams,
        ) -> Result<AgentApiOutcome<SkillActiveResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(SkillActiveResponse {
                catalog_ref: Some(format!("sha256:{}", "5".repeat(64))),
                activations: vec![test_skill_activation(SkillActivationScope::Run)],
            }))
        }

        async fn activate_skill(
            &self,
            params: SkillActivateParams,
        ) -> Result<AgentApiOutcome<SkillActivateResponse>, AgentApiError> {
            assert_eq!(params.skill_id, "skill:one");
            let activation = test_skill_activation(params.scope);
            Ok(AgentApiOutcome::new(SkillActivateResponse {
                activation: activation.clone(),
                active: vec![activation],
            }))
        }

        async fn deactivate_skill(
            &self,
            params: SkillDeactivateParams,
        ) -> Result<AgentApiOutcome<SkillDeactivateResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(SkillDeactivateResponse {
                skill_id: params.skill_id,
                active: Vec::new(),
            }))
        }

        async fn list_session_environments(
            &self,
            _params: SessionEnvironmentListParams,
        ) -> Result<AgentApiOutcome<SessionEnvironmentListResponse>, AgentApiError> {
            let environment = test_session_environment(true);
            Ok(AgentApiOutcome::new(SessionEnvironmentListResponse {
                active_env_id: Some(environment.env_id.clone()),
                environments: vec![environment],
            }))
        }

        async fn read_session_environment(
            &self,
            params: SessionEnvironmentReadParams,
        ) -> Result<AgentApiOutcome<SessionEnvironmentReadResponse>, AgentApiError> {
            assert_eq!(params.env_id, "test");
            Ok(AgentApiOutcome::new(SessionEnvironmentReadResponse {
                environment: test_session_environment(true),
            }))
        }

        async fn create_session_environment(
            &self,
            params: SessionEnvironmentCreateParams,
        ) -> Result<AgentApiOutcome<SessionEnvironmentCreateResponse>, AgentApiError> {
            let mut environment = test_session_environment(params.activate);
            environment.env_id = params.env_id.unwrap_or_else(|| "created".to_owned());
            Ok(AgentApiOutcome::new(SessionEnvironmentCreateResponse {
                active_env_id: params.activate.then(|| environment.env_id.clone()),
                environments: vec![environment.clone()],
                environment,
            }))
        }

        async fn attach_session_environment(
            &self,
            params: SessionEnvironmentAttachParams,
        ) -> Result<AgentApiOutcome<SessionEnvironmentAttachResponse>, AgentApiError> {
            let mut environment = test_session_environment(params.activate);
            environment.env_id = params.env_id.unwrap_or_else(|| "attached".to_owned());
            Ok(AgentApiOutcome::new(SessionEnvironmentAttachResponse {
                active_env_id: params.activate.then(|| environment.env_id.clone()),
                environments: vec![environment.clone()],
                environment,
            }))
        }

        async fn activate_session_environment(
            &self,
            params: SessionEnvironmentActivateParams,
        ) -> Result<AgentApiOutcome<SessionEnvironmentActivateResponse>, AgentApiError> {
            assert_eq!(params.env_id, "test");
            let environment = test_session_environment(true);
            Ok(AgentApiOutcome::new(SessionEnvironmentActivateResponse {
                active_env_id: Some(environment.env_id.clone()),
                environments: vec![environment.clone()],
                environment,
            }))
        }

        async fn deactivate_session_environment(
            &self,
            _params: SessionEnvironmentDeactivateParams,
        ) -> Result<AgentApiOutcome<SessionEnvironmentDeactivateResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(SessionEnvironmentDeactivateResponse {
                active_env_id: None,
                environments: vec![test_session_environment(false)],
            }))
        }

        async fn close_session_environment(
            &self,
            _params: SessionEnvironmentCloseParams,
        ) -> Result<AgentApiOutcome<SessionEnvironmentCloseResponse>, AgentApiError> {
            let mut environment = test_session_environment(false);
            environment.status = SessionEnvironmentStatusView::Detached;
            Ok(AgentApiOutcome::new(SessionEnvironmentCloseResponse {
                active_env_id: None,
                environments: vec![environment.clone()],
                environment,
            }))
        }

        async fn register_environment_provider(
            &self,
            params: EnvironmentProviderRegisterParams,
        ) -> Result<AgentApiOutcome<EnvironmentProviderRegisterResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(EnvironmentProviderRegisterResponse {
                provider: test_environment_provider(
                    params.provider_id,
                    params.provider_kind,
                    EnvironmentProviderStatusView::Online,
                ),
            }))
        }

        async fn heartbeat_environment_provider(
            &self,
            params: EnvironmentProviderHeartbeatParams,
        ) -> Result<AgentApiOutcome<EnvironmentProviderHeartbeatResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(EnvironmentProviderHeartbeatResponse {
                provider: test_environment_provider(
                    params.provider_id,
                    EnvironmentProviderKindView::Bridge,
                    EnvironmentProviderStatusView::Online,
                ),
                targets: params.observed_targets,
            }))
        }

        async fn unregister_environment_provider(
            &self,
            params: EnvironmentProviderUnregisterParams,
        ) -> Result<AgentApiOutcome<EnvironmentProviderUnregisterResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(
                EnvironmentProviderUnregisterResponse {
                    provider: test_environment_provider(
                        params.provider_id,
                        EnvironmentProviderKindView::Bridge,
                        EnvironmentProviderStatusView::Offline,
                    ),
                },
            ))
        }

        async fn put_blob(
            &self,
            params: BlobPutParams,
        ) -> Result<AgentApiOutcome<BlobPutResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(BlobPutResponse {
                blob_ref: format!("sha256:{}", "1".repeat(64)),
                bytes: params.bytes_base64.len() as u64,
            }))
        }

        async fn put_blobs(
            &self,
            params: BlobPutManyParams,
        ) -> Result<AgentApiOutcome<BlobPutManyResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(BlobPutManyResponse {
                blobs: params
                    .blobs
                    .into_iter()
                    .enumerate()
                    .map(|(index, blob)| BlobPutResponse {
                        blob_ref: format!("sha256:{index:064x}"),
                        bytes: blob.bytes_base64.len() as u64,
                    })
                    .collect(),
            }))
        }

        async fn get_blob(
            &self,
            params: BlobGetParams,
        ) -> Result<AgentApiOutcome<BlobGetResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(BlobGetResponse {
                blob_ref: params.blob_ref,
                bytes_base64: "aGVsbG8=".to_owned(),
                bytes: 5,
            }))
        }

        async fn has_blobs(
            &self,
            params: BlobHasManyParams,
        ) -> Result<AgentApiOutcome<BlobHasManyResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(BlobHasManyResponse {
                blobs: params
                    .blob_refs
                    .into_iter()
                    .map(|blob_ref| BlobHasItem {
                        blob_ref,
                        exists: true,
                    })
                    .collect(),
            }))
        }

        async fn commit_vfs_snapshot(
            &self,
            _params: VfsSnapshotCommitParams,
        ) -> Result<AgentApiOutcome<VfsSnapshotCommitResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(VfsSnapshotCommitResponse {
                snapshot_ref: format!("sha256:{}", "2".repeat(64)),
                files: 1,
                bytes: 5,
            }))
        }

        async fn read_vfs_snapshot(
            &self,
            params: VfsSnapshotReadParams,
        ) -> Result<AgentApiOutcome<VfsSnapshotReadResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(VfsSnapshotReadResponse {
                snapshot_ref: params.snapshot_ref,
                manifest: json!({
                    "schema_version": "lightspeed.vfs.snapshot.v1",
                    "root": { "entries": {} },
                    "totals": { "files": 0, "bytes": 0 }
                }),
                files: 0,
                bytes: 0,
            }))
        }

        async fn create_vfs_workspace(
            &self,
            params: VfsWorkspaceCreateParams,
        ) -> Result<AgentApiOutcome<VfsWorkspaceCreateResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(VfsWorkspaceCreateResponse {
                workspace: VfsWorkspaceView {
                    workspace_id: params
                        .workspace_id
                        .unwrap_or_else(|| "workspace_test".to_owned()),
                    base_snapshot_ref: Some(params.snapshot_ref.clone()),
                    head_snapshot_ref: params.snapshot_ref,
                    revision: 0,
                },
            }))
        }

        async fn read_vfs_workspace(
            &self,
            params: VfsWorkspaceReadParams,
        ) -> Result<AgentApiOutcome<VfsWorkspaceReadResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(VfsWorkspaceReadResponse {
                workspace: VfsWorkspaceView {
                    workspace_id: params.workspace_id,
                    base_snapshot_ref: Some(format!("sha256:{}", "2".repeat(64))),
                    head_snapshot_ref: format!("sha256:{}", "3".repeat(64)),
                    revision: 4,
                },
            }))
        }

        async fn update_vfs_workspace(
            &self,
            params: VfsWorkspaceUpdateParams,
        ) -> Result<AgentApiOutcome<VfsWorkspaceUpdateResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(VfsWorkspaceUpdateResponse {
                workspace: VfsWorkspaceView {
                    workspace_id: params.workspace_id,
                    base_snapshot_ref: Some(format!("sha256:{}", "2".repeat(64))),
                    head_snapshot_ref: params.snapshot_ref,
                    revision: params.expected_revision.unwrap_or(4) + 1,
                },
            }))
        }

        async fn delete_vfs_workspace(
            &self,
            params: VfsWorkspaceDeleteParams,
        ) -> Result<AgentApiOutcome<VfsWorkspaceDeleteResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(VfsWorkspaceDeleteResponse {
                workspace: VfsWorkspaceView {
                    workspace_id: params.workspace_id,
                    base_snapshot_ref: Some(format!("sha256:{}", "2".repeat(64))),
                    head_snapshot_ref: format!("sha256:{}", "3".repeat(64)),
                    revision: 4,
                },
            }))
        }

        async fn put_vfs_mount(
            &self,
            params: VfsMountPutParams,
        ) -> Result<AgentApiOutcome<VfsMountPutResponse>, AgentApiError> {
            let mount = VfsMountView {
                mount_path: params.mount_path,
                source: match params.source {
                    VfsMountSourceInput::Snapshot { snapshot_ref } => {
                        VfsMountSourceView::Snapshot { snapshot_ref }
                    }
                    VfsMountSourceInput::Workspace { workspace_id } => {
                        VfsMountSourceView::Workspace {
                            workspace_id,
                            head_snapshot_ref: Some(format!("sha256:{}", "3".repeat(64))),
                            revision: Some(0),
                        }
                    }
                },
                access: params.access,
            };
            Ok(AgentApiOutcome::new(VfsMountPutResponse {
                mount: mount.clone(),
                session: SessionView {
                    vfs_mounts: vec![mount],
                    ..test_session(params.session_id, SessionStatus::Idle)
                },
            }))
        }

        async fn delete_vfs_mount(
            &self,
            params: VfsMountDeleteParams,
        ) -> Result<AgentApiOutcome<VfsMountDeleteResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(VfsMountDeleteResponse {
                mount_path: params.mount_path,
                session: test_session(params.session_id, SessionStatus::Idle),
            }))
        }

        async fn list_vfs_mounts(
            &self,
            params: VfsMountListParams,
        ) -> Result<AgentApiOutcome<VfsMountListResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(VfsMountListResponse {
                mounts: vec![VfsMountView {
                    mount_path: "/workspace".to_owned(),
                    source: VfsMountSourceView::Workspace {
                        workspace_id: format!("workspace_{}", params.session_id),
                        head_snapshot_ref: Some(format!("sha256:{}", "3".repeat(64))),
                        revision: Some(0),
                    },
                    access: VfsMountAccess::ReadWrite,
                }],
            }))
        }

        async fn create_mcp_server(
            &self,
            params: McpServerCreateParams,
        ) -> Result<AgentApiOutcome<McpServerCreateResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(McpServerCreateResponse {
                server: test_mcp_server(params.server_id),
            }))
        }

        async fn list_mcp_servers(
            &self,
            _params: McpServerListParams,
        ) -> Result<AgentApiOutcome<McpServerListResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(McpServerListResponse {
                servers: vec![test_mcp_server("echo".to_owned())],
            }))
        }

        async fn read_mcp_server(
            &self,
            params: McpServerReadParams,
        ) -> Result<AgentApiOutcome<McpServerReadResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(McpServerReadResponse {
                server: test_mcp_server(params.server_id),
            }))
        }

        async fn delete_mcp_server(
            &self,
            params: McpServerDeleteParams,
        ) -> Result<AgentApiOutcome<McpServerDeleteResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(McpServerDeleteResponse {
                server: test_mcp_server(params.server_id),
            }))
        }

        async fn link_session_mcp(
            &self,
            params: SessionMcpLinkParams,
        ) -> Result<AgentApiOutcome<SessionMcpLinkResponse>, AgentApiError> {
            let link = test_mcp_link(params.tool_id.unwrap_or_else(|| "mcp_echo".to_owned()));
            Ok(AgentApiOutcome::new(SessionMcpLinkResponse {
                link: link.clone(),
                links: vec![link],
                session: test_session(params.session_id, SessionStatus::Idle),
            }))
        }

        async fn unlink_session_mcp(
            &self,
            params: SessionMcpUnlinkParams,
        ) -> Result<AgentApiOutcome<SessionMcpUnlinkResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(SessionMcpUnlinkResponse {
                tool_id: params.tool_id,
                links: Vec::new(),
                session: test_session(params.session_id, SessionStatus::Idle),
            }))
        }

        async fn list_session_mcp(
            &self,
            _params: SessionMcpListParams,
        ) -> Result<AgentApiOutcome<SessionMcpListResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(SessionMcpListResponse {
                links: vec![test_mcp_link("mcp_echo".to_owned())],
            }))
        }

        async fn import_auth_grant(
            &self,
            params: AuthGrantImportParams,
        ) -> Result<AgentApiOutcome<AuthGrantImportResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthGrantImportResponse {
                grant: test_auth_grant(
                    params.grant_id.unwrap_or_else(|| "authgrant_1".to_owned()),
                    AuthGrantStatus::Active,
                ),
            }))
        }

        async fn list_auth_grants(
            &self,
            _params: AuthGrantListParams,
        ) -> Result<AgentApiOutcome<AuthGrantListResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthGrantListResponse {
                grants: vec![test_auth_grant(
                    "authgrant_1".to_owned(),
                    AuthGrantStatus::Active,
                )],
            }))
        }

        async fn read_auth_grant(
            &self,
            params: AuthGrantReadParams,
        ) -> Result<AgentApiOutcome<AuthGrantReadResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthGrantReadResponse {
                grant: test_auth_grant(params.grant_id, AuthGrantStatus::Active),
            }))
        }

        async fn revoke_auth_grant(
            &self,
            params: AuthGrantRevokeParams,
        ) -> Result<AgentApiOutcome<AuthGrantRevokeResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthGrantRevokeResponse {
                grant: test_auth_grant(params.grant_id, AuthGrantStatus::Revoked),
            }))
        }

        async fn create_auth_client(
            &self,
            params: AuthClientCreateParams,
        ) -> Result<AgentApiOutcome<AuthClientCreateResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthClientCreateResponse {
                client: test_auth_client(params.client_id.unwrap_or_else(|| "crm".to_owned())),
            }))
        }

        async fn list_auth_clients(
            &self,
            _params: AuthClientListParams,
        ) -> Result<AgentApiOutcome<AuthClientListResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthClientListResponse {
                clients: vec![test_auth_client("crm".to_owned())],
            }))
        }

        async fn read_auth_client(
            &self,
            params: AuthClientReadParams,
        ) -> Result<AgentApiOutcome<AuthClientReadResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthClientReadResponse {
                client: test_auth_client(params.client_id),
            }))
        }

        async fn delete_auth_client(
            &self,
            params: AuthClientDeleteParams,
        ) -> Result<AgentApiOutcome<AuthClientDeleteResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthClientDeleteResponse {
                client: test_auth_client(params.client_id),
            }))
        }

        async fn start_auth_flow(
            &self,
            params: AuthFlowStartParams,
        ) -> Result<AgentApiOutcome<AuthFlowStartResponse>, AgentApiError> {
            let _ = params;
            Ok(AgentApiOutcome::new(AuthFlowStartResponse {
                flow_id: "authflow_1".to_owned(),
                authorize_url: "https://as.example.com/authorize?state=test".to_owned(),
                expires_at_ms: 600_000,
            }))
        }

        async fn read_auth_flow_status(
            &self,
            params: AuthFlowStatusParams,
        ) -> Result<AgentApiOutcome<AuthFlowStatusResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthFlowStatusResponse {
                flow: AuthFlowView {
                    flow_id: params.flow_id,
                    client_id: "crm".to_owned(),
                    provider_id: "crm".to_owned(),
                    status: AuthFlowStatus::Pending,
                    grant_id: None,
                    error: None,
                    expires_at_ms: 600_000,
                    created_at_ms: 1,
                    updated_at_ms: 2,
                },
            }))
        }

        async fn create_auth_provider(
            &self,
            params: AuthProviderCreateParams,
        ) -> Result<AgentApiOutcome<AuthProviderCreateResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthProviderCreateResponse {
                provider: test_auth_provider(
                    params
                        .provider_id
                        .unwrap_or_else(|| "lightspeed-github".to_owned()),
                ),
            }))
        }

        async fn list_auth_providers(
            &self,
            _params: AuthProviderListParams,
        ) -> Result<AgentApiOutcome<AuthProviderListResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthProviderListResponse {
                providers: vec![test_auth_provider("lightspeed-github".to_owned())],
            }))
        }

        async fn read_auth_provider(
            &self,
            params: AuthProviderReadParams,
        ) -> Result<AgentApiOutcome<AuthProviderReadResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthProviderReadResponse {
                provider: test_auth_provider(params.provider_id),
            }))
        }

        async fn delete_auth_provider(
            &self,
            params: AuthProviderDeleteParams,
        ) -> Result<AgentApiOutcome<AuthProviderDeleteResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthProviderDeleteResponse {
                provider: test_auth_provider(params.provider_id),
            }))
        }

        async fn list_github_installations(
            &self,
            _params: AuthGitHubInstallationListParams,
        ) -> Result<AgentApiOutcome<AuthGitHubInstallationListResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthGitHubInstallationListResponse {
                installations: vec![GitHubInstallationView {
                    installation_id: 678,
                    account_login: Some("acme".to_owned()),
                    repository_selection: Some("selected".to_owned()),
                    permissions: serde_json::json!({"contents": "read"}),
                }],
            }))
        }

        async fn grant_github_installation(
            &self,
            _params: AuthGitHubInstallationGrantParams,
        ) -> Result<AgentApiOutcome<AuthGitHubInstallationGrantResponse>, AgentApiError> {
            Ok(AgentApiOutcome::new(AuthGitHubInstallationGrantResponse {
                grant: test_auth_grant("authgrant_install".to_owned(), AuthGrantStatus::Active),
            }))
        }
    }

    fn test_auth_provider(provider_id: String) -> AuthProviderView {
        AuthProviderView {
            provider_id,
            provider_kind: AuthProviderKind::GitHubApp,
            display_name: None,
            config: AuthProviderConfigView::GitHubApp {
                app_id: "12345".to_owned(),
                api_base_url: "https://api.github.com".to_owned(),
            },
            has_credential: true,
            status: AuthProviderStatus::Active,
            created_at_ms: 1,
            updated_at_ms: 2,
        }
    }

    fn test_auth_client(client_id: String) -> OAuthClientView {
        OAuthClientView {
            client_id,
            provider_id: "crm".to_owned(),
            provider_kind: AuthProviderKind::McpOAuth,
            display_name: None,
            authorization_endpoint: "https://as.example.com/authorize".to_owned(),
            token_endpoint: "https://as.example.com/token".to_owned(),
            remote_client_id: "client-1".to_owned(),
            has_client_secret: false,
            token_endpoint_auth_method: TokenEndpointAuthMethod::None,
            scopes_default: Vec::new(),
            audience: Some("https://crm.example.com/mcp".to_owned()),
            created_at_ms: 1,
            updated_at_ms: 2,
        }
    }

    fn test_auth_grant(grant_id: String, status: AuthGrantStatus) -> AuthGrantView {
        AuthGrantView {
            grant_id,
            provider_id: "static".to_owned(),
            provider_kind: AuthProviderKind::StaticBearer,
            principal: PrincipalRefView::default(),
            display_name: None,
            subject_hint: None,
            scopes: Vec::new(),
            audience: None,
            has_access_token: true,
            has_refresh_token: false,
            expires_at_ms: None,
            status,
            metadata: serde_json::Value::Object(Default::default()),
            created_at_ms: 1,
            updated_at_ms: 2,
        }
    }

    fn test_session(id: SessionId, status: SessionStatus) -> SessionView {
        SessionView {
            id,
            status,
            cwd: None,
            config_revision: 0,
            config: None,
            created_at_ms: 1,
            updated_at_ms: 2,
            runs: Vec::new(),
            active_context: ContextView::default(),
            active_tools: ActiveToolsView::default(),
            vfs_mounts: Vec::new(),
        }
    }

    fn test_session_environment(active: bool) -> SessionEnvironmentView {
        SessionEnvironmentView {
            env_id: "test".to_owned(),
            kind: SessionEnvironmentKindView::AttachedHost,
            status: SessionEnvironmentStatusView::Ready,
            capabilities: SessionEnvironmentCapabilitiesView {
                fs_read: true,
                fs_write: true,
                process_exec: true,
                process_stdin: true,
                network: false,
                persistent: false,
            },
            exec_target: Some(ToolExecutionTargetView {
                namespace: "env".to_owned(),
                id: "test".to_owned(),
            }),
            cwd: Some("/workspace".to_owned()),
            active,
        }
    }

    fn test_environment_provider(
        provider_id: EnvironmentProviderId,
        provider_kind: EnvironmentProviderKindView,
        status: EnvironmentProviderStatusView,
    ) -> EnvironmentProviderView {
        EnvironmentProviderView {
            provider_id,
            provider_kind,
            status,
            controller_connection: HostControllerConnectionView {
                endpoint: "ws://127.0.0.1:9000/controller".to_owned(),
                transport: HostTransportView::WebSocket,
            },
            capabilities: EnvironmentProviderCapabilitiesView {
                list_targets: true,
                attach_target: true,
                get_target: true,
                ..EnvironmentProviderCapabilitiesView::default()
            },
            implementation: EnvironmentProviderImplementationView {
                name: "test-bridge".to_owned(),
                version: Some("1.0.0".to_owned()),
            },
            last_seen_ms: 10,
            lease_expires_ms: 30_010,
            display_name: Some("Local bridge".to_owned()),
            metadata: BTreeMap::new(),
        }
    }

    fn test_run(id: RunId, status: RunStatus) -> RunView {
        RunView {
            id,
            status,
            input: Vec::new(),
            items: Vec::new(),
            tool_batches: Vec::new(),
        }
    }

    fn test_skill_activation(scope: SkillActivationScope) -> SkillActivationView {
        SkillActivationView {
            skill_id: "skill:one".to_owned(),
            name: Some("one".to_owned()),
            description: Some("Use when testing skills.".to_owned()),
            short_description: Some("test skill".to_owned()),
            catalog_ref: format!("sha256:{}", "5".repeat(64)),
            scope,
            source: SkillActivationSource::DirectContext {
                context_ref: format!("sha256:{}", "6".repeat(64)),
            },
        }
    }

    fn test_mcp_server(server_id: String) -> McpServerView {
        McpServerView {
            default_server_label: server_id.clone(),
            server_url: format!("https://{server_id}.example.com/mcp"),
            server_id,
            display_name: None,
            transport: RemoteMcpTransport::Auto,
            description: None,
            allowed_tools: None,
            approval_default: RemoteMcpApprovalPolicy::ProviderDefault,
            defer_loading_default: None,
            auth_policy: McpServerAuthPolicy::None,
            status: McpServerStatus::Active,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    fn test_mcp_link(tool_id: String) -> SessionMcpLinkView {
        SessionMcpLinkView {
            tool_id,
            server_label: "echo".to_owned(),
            server_url: "https://echo.example.com/mcp".to_owned(),
            allowed_tools: None,
            approval: RemoteMcpApprovalPolicy::ProviderDefault,
            defer_loading: None,
            auth_ref: None,
        }
    }
}
