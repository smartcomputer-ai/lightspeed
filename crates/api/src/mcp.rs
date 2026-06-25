use super::*;

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
