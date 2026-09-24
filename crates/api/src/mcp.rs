use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerView {
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub server_url: String,
    pub default_server_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    pub execution: RemoteMcpExecution,
    pub exposure: RemoteMcpExposure,
    pub approval: RemoteMcpApprovalPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    pub allow_private_network: bool,
    pub auth_policy: McpServerAuthPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<McpServerCredential>,
    pub status: McpServerStatus,
    pub revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// The server's owner and visibility; a server runs no work of its own,
    /// so `execution` is absent.
    pub access: ResourceAccessSummary,
}

/// Read-only authentication discovery for a prospective MCP endpoint. A
/// missing OAuth result is deliberately inconclusive: the server may be
/// public, use bearer auth, or expose OAuth metadata only through an explicit
/// URL. No catalog or auth records are created by this probe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerAuthDiscoverParams {
    pub server_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthDiscoveryView {
    pub resource: String,
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    /// Scopes advertised by current protected-resource/authorization-server
    /// metadata. Advisory only; clients must not silently expand consent.
    #[serde(default)]
    pub scopes_supported: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerAuthDiscoverResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthDiscoveryView>,
}

/// Live, non-persisting tool discovery for a configured MCP server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerToolsDiscoverParams {
    pub server_id: String,
}

/// The result of one live MCP `tools/list` exchange. Operational connection
/// failures are data so management clients can render stable remediation;
/// ordinary API admission/not-found/internal failures remain API errors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum McpServerToolsDiscoverResponse {
    Success {
        tools: Vec<McpAdvertisedToolView>,
    },
    Failure {
        code: McpToolDiscoveryFailureCode,
        message: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        required_scopes: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpAdvertisedToolView {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<McpToolAnnotationsView>,
}

/// Narrow projection of standard MCP `ToolAnnotations`. All values are
/// untrusted hints and never authorization facts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpToolAnnotationsView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum McpToolDiscoveryFailureCode {
    CredentialAbsent,
    GrantNeedsReauth,
    GrantAudienceMismatch,
    Unauthorized,
    Forbidden,
    AdditionalConsentRequired,
    RemoteRateLimited,
    RemoteFailure,
    Unreachable,
    InvalidResponse,
    UnsupportedProtocol,
    PaginationLimit,
    ResponseTooLarge,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RemoteMcpApprovalPolicy {
    #[default]
    Never,
    Always,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RemoteMcpExecution {
    #[default]
    Provider,
    Native,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RemoteMcpExposure {
    #[default]
    Inject,
    Search,
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

/// Non-secret universe credential selected by this configured MCP server.
/// Token material remains in the auth subsystem and is never returned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum McpServerCredential {
    AuthGrant { grant_id: String },
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

/// Full MCP server document as submitted by clients.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerInput {
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub server_url: String,
    pub default_server_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub execution: RemoteMcpExecution,
    #[serde(default)]
    pub exposure: RemoteMcpExposure,
    /// Approval policy for every session linking this server.
    #[serde(default)]
    pub approval: RemoteMcpApprovalPolicy,
    /// Provider-side deferred loading of tool definitions where supported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    #[serde(default)]
    pub allow_private_network: bool,
    #[serde(default)]
    pub auth_policy: McpServerAuthPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<McpServerCredential>,
    #[serde(default)]
    pub status: McpServerStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerPutParams {
    pub server: McpServerInput,
    /// Checked only when the server already exists; absent replaces (or
    /// creates) unconditionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    /// Who may see and use a server this call creates; the caller owns it.
    /// Absent means universe-visible. Grants take the `use` permission only.
    /// Refused when the server already exists: change its access with
    /// `access/policy/put`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerPutResponse {
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
