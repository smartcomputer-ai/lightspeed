use std::sync::atomic::{AtomicU64, Ordering};

use api::{
    AgentApiError, AgentApiErrorKind, AgentApiOutcome, AuthClientCreateParams,
    AuthClientCreateResponse, AuthClientDeleteParams, AuthClientDeleteResponse,
    AuthClientListParams, AuthClientListResponse, AuthClientReadParams, AuthClientReadResponse,
    AuthFlowStartParams, AuthFlowStartResponse, AuthFlowStatusParams, AuthFlowStatusResponse,
    AuthGitHubInstallationGrantParams, AuthGitHubInstallationGrantResponse,
    AuthGitHubInstallationListParams, AuthGitHubInstallationListResponse, AuthGrantImportParams,
    AuthGrantImportResponse, AuthGrantListParams, AuthGrantListResponse, AuthGrantReadParams,
    AuthGrantReadResponse, AuthGrantRevokeParams, AuthGrantRevokeResponse,
    AuthProviderCreateParams, AuthProviderCreateResponse, AuthProviderDeleteParams,
    AuthProviderDeleteResponse, AuthProviderListParams, AuthProviderListResponse,
    AuthProviderReadParams, AuthProviderReadResponse, BlobGetParams, BlobGetResponse,
    BlobHasManyParams, BlobHasManyResponse, BlobPutManyParams, BlobPutManyResponse, JsonRpcRequest,
    JsonRpcResponse, METHOD_AUTH_CLIENTS_CREATE, METHOD_AUTH_CLIENTS_DELETE,
    METHOD_AUTH_CLIENTS_LIST, METHOD_AUTH_CLIENTS_READ, METHOD_AUTH_FLOWS_START,
    METHOD_AUTH_FLOWS_STATUS, METHOD_AUTH_GITHUB_INSTALLATIONS_GRANT,
    METHOD_AUTH_GITHUB_INSTALLATIONS_LIST, METHOD_AUTH_GRANTS_IMPORT, METHOD_AUTH_GRANTS_LIST,
    METHOD_AUTH_GRANTS_READ, METHOD_AUTH_GRANTS_REVOKE, METHOD_AUTH_PROVIDERS_CREATE,
    METHOD_AUTH_PROVIDERS_DELETE, METHOD_AUTH_PROVIDERS_LIST, METHOD_AUTH_PROVIDERS_READ,
    METHOD_BLOB_GET, METHOD_BLOB_HAS_MANY, METHOD_BLOB_PUT_MANY, METHOD_MCP_SERVERS_CREATE,
    METHOD_MCP_SERVERS_DELETE, METHOD_MCP_SERVERS_LIST, METHOD_MCP_SERVERS_READ,
    METHOD_PROFILES_APPLY, METHOD_PROFILES_CREATE, METHOD_PROFILES_DELETE, METHOD_PROFILES_LIST,
    METHOD_PROFILES_READ, METHOD_PROFILES_UPDATE, METHOD_RUN_START,
    METHOD_SESSION_ENVIRONMENTS_ACTIVATE, METHOD_SESSION_ENVIRONMENTS_ATTACH,
    METHOD_SESSION_ENVIRONMENTS_CLOSE, METHOD_SESSION_ENVIRONMENTS_DEACTIVATE,
    METHOD_SESSION_ENVIRONMENTS_LIST, METHOD_SESSION_ENVIRONMENTS_READ, METHOD_SESSION_EVENTS_READ,
    METHOD_SESSION_MCP_LINK, METHOD_SESSION_MCP_LIST, METHOD_SESSION_MCP_UNLINK,
    METHOD_SESSION_READ, METHOD_SESSION_START, METHOD_SKILLS_ACTIVATE, METHOD_SKILLS_ACTIVE,
    METHOD_SKILLS_DEACTIVATE, METHOD_SKILLS_LIST, METHOD_VFS_MOUNT_DELETE, METHOD_VFS_MOUNT_LIST,
    METHOD_VFS_MOUNT_PUT, METHOD_VFS_SNAPSHOT_COMMIT, METHOD_VFS_SNAPSHOT_READ,
    METHOD_VFS_WORKSPACE_CREATE, METHOD_VFS_WORKSPACE_DELETE, METHOD_VFS_WORKSPACE_READ,
    METHOD_VFS_WORKSPACE_UPDATE, McpServerCreateParams, McpServerCreateResponse,
    McpServerDeleteParams, McpServerDeleteResponse, McpServerListParams, McpServerListResponse,
    McpServerReadParams, McpServerReadResponse, ProfileApplyParams, ProfileApplyResponse,
    ProfileCreateParams, ProfileCreateResponse, ProfileDeleteParams, ProfileDeleteResponse,
    ProfileListParams, ProfileListResponse, ProfileReadParams, ProfileReadResponse,
    ProfileUpdateParams, ProfileUpdateResponse, RequestId, RunStartParams, RunStartResponse,
    SessionEnvironmentActivateParams, SessionEnvironmentActivateResponse,
    SessionEnvironmentAttachParams, SessionEnvironmentAttachResponse,
    SessionEnvironmentCloseParams, SessionEnvironmentCloseResponse,
    SessionEnvironmentDeactivateParams, SessionEnvironmentDeactivateResponse,
    SessionEnvironmentListParams, SessionEnvironmentListResponse, SessionEnvironmentReadParams,
    SessionEnvironmentReadResponse, SessionEventsReadParams, SessionEventsReadResponse,
    SessionMcpLinkParams, SessionMcpLinkResponse, SessionMcpListParams, SessionMcpListResponse,
    SessionMcpUnlinkParams, SessionMcpUnlinkResponse, SessionReadParams, SessionReadResponse,
    SessionStartParams, SessionStartResponse, SkillActivateParams, SkillActivateResponse,
    SkillActiveParams, SkillActiveResponse, SkillDeactivateParams, SkillDeactivateResponse,
    SkillListParams, SkillListResponse, VfsMountDeleteParams, VfsMountDeleteResponse,
    VfsMountListParams, VfsMountListResponse, VfsMountPutParams, VfsMountPutResponse,
    VfsSnapshotCommitParams, VfsSnapshotCommitResponse, VfsSnapshotReadParams,
    VfsSnapshotReadResponse, VfsWorkspaceCreateParams, VfsWorkspaceCreateResponse,
    VfsWorkspaceDeleteParams, VfsWorkspaceDeleteResponse, VfsWorkspaceReadParams,
    VfsWorkspaceReadResponse, VfsWorkspaceUpdateParams, VfsWorkspaceUpdateResponse,
};
use serde::{Serialize, de::DeserializeOwned};

pub(crate) struct HttpAgentApi {
    endpoint: String,
    client: reqwest::Client,
    next_id: AtomicU64,
}

impl HttpAgentApi {
    pub(crate) fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: reqwest::Client::new(),
            next_id: AtomicU64::new(1),
        }
    }

    pub(crate) async fn open_or_start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        match self.start_session(params.clone()).await {
            Ok(outcome) => Ok(outcome),
            Err(error)
                if matches!(error.kind, AgentApiErrorKind::Conflict)
                    && params.session_id.is_some() =>
            {
                self.read_session(SessionReadParams {
                    session_id: params.session_id.expect("checked session id present"),
                })
                .await
                .map(|outcome| {
                    AgentApiOutcome::with_notifications(
                        SessionStartResponse {
                            session: outcome.result.session,
                        },
                        outcome.notifications,
                    )
                })
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        self.request(METHOD_SESSION_START, params).await
    }

    pub(crate) async fn create_profile(
        &self,
        params: ProfileCreateParams,
    ) -> Result<AgentApiOutcome<ProfileCreateResponse>, AgentApiError> {
        self.request(METHOD_PROFILES_CREATE, params).await
    }

    pub(crate) async fn read_profile(
        &self,
        params: ProfileReadParams,
    ) -> Result<AgentApiOutcome<ProfileReadResponse>, AgentApiError> {
        self.request(METHOD_PROFILES_READ, params).await
    }

    pub(crate) async fn list_profiles(
        &self,
        params: ProfileListParams,
    ) -> Result<AgentApiOutcome<ProfileListResponse>, AgentApiError> {
        self.request(METHOD_PROFILES_LIST, params).await
    }

    pub(crate) async fn update_profile(
        &self,
        params: ProfileUpdateParams,
    ) -> Result<AgentApiOutcome<ProfileUpdateResponse>, AgentApiError> {
        self.request(METHOD_PROFILES_UPDATE, params).await
    }

    pub(crate) async fn delete_profile(
        &self,
        params: ProfileDeleteParams,
    ) -> Result<AgentApiOutcome<ProfileDeleteResponse>, AgentApiError> {
        self.request(METHOD_PROFILES_DELETE, params).await
    }

    pub(crate) async fn apply_profile(
        &self,
        params: ProfileApplyParams,
    ) -> Result<AgentApiOutcome<ProfileApplyResponse>, AgentApiError> {
        self.request(METHOD_PROFILES_APPLY, params).await
    }

    pub(crate) async fn read_session(
        &self,
        params: SessionReadParams,
    ) -> Result<AgentApiOutcome<SessionReadResponse>, AgentApiError> {
        self.request(METHOD_SESSION_READ, params).await
    }

    pub(crate) async fn read_session_events(
        &self,
        params: SessionEventsReadParams,
    ) -> Result<AgentApiOutcome<SessionEventsReadResponse>, AgentApiError> {
        self.request(METHOD_SESSION_EVENTS_READ, params).await
    }

    pub(crate) async fn start_run(
        &self,
        params: RunStartParams,
    ) -> Result<AgentApiOutcome<RunStartResponse>, AgentApiError> {
        self.request(METHOD_RUN_START, params).await
    }

    pub(crate) async fn list_skills(
        &self,
        params: SkillListParams,
    ) -> Result<AgentApiOutcome<SkillListResponse>, AgentApiError> {
        self.request(METHOD_SKILLS_LIST, params).await
    }

    pub(crate) async fn active_skills(
        &self,
        params: SkillActiveParams,
    ) -> Result<AgentApiOutcome<SkillActiveResponse>, AgentApiError> {
        self.request(METHOD_SKILLS_ACTIVE, params).await
    }

    pub(crate) async fn activate_skill(
        &self,
        params: SkillActivateParams,
    ) -> Result<AgentApiOutcome<SkillActivateResponse>, AgentApiError> {
        self.request(METHOD_SKILLS_ACTIVATE, params).await
    }

    pub(crate) async fn deactivate_skill(
        &self,
        params: SkillDeactivateParams,
    ) -> Result<AgentApiOutcome<SkillDeactivateResponse>, AgentApiError> {
        self.request(METHOD_SKILLS_DEACTIVATE, params).await
    }

    pub(crate) async fn put_blobs(
        &self,
        params: BlobPutManyParams,
    ) -> Result<AgentApiOutcome<BlobPutManyResponse>, AgentApiError> {
        self.request(METHOD_BLOB_PUT_MANY, params).await
    }

    pub(crate) async fn has_blobs(
        &self,
        params: BlobHasManyParams,
    ) -> Result<AgentApiOutcome<BlobHasManyResponse>, AgentApiError> {
        self.request(METHOD_BLOB_HAS_MANY, params).await
    }

    pub(crate) async fn get_blob(
        &self,
        params: BlobGetParams,
    ) -> Result<AgentApiOutcome<BlobGetResponse>, AgentApiError> {
        self.request(METHOD_BLOB_GET, params).await
    }

    pub(crate) async fn commit_vfs_snapshot(
        &self,
        params: VfsSnapshotCommitParams,
    ) -> Result<AgentApiOutcome<VfsSnapshotCommitResponse>, AgentApiError> {
        self.request(METHOD_VFS_SNAPSHOT_COMMIT, params).await
    }

    pub(crate) async fn read_vfs_snapshot(
        &self,
        params: VfsSnapshotReadParams,
    ) -> Result<AgentApiOutcome<VfsSnapshotReadResponse>, AgentApiError> {
        self.request(METHOD_VFS_SNAPSHOT_READ, params).await
    }

    pub(crate) async fn create_vfs_workspace(
        &self,
        params: VfsWorkspaceCreateParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceCreateResponse>, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACE_CREATE, params).await
    }

    pub(crate) async fn read_vfs_workspace(
        &self,
        params: VfsWorkspaceReadParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceReadResponse>, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACE_READ, params).await
    }

    pub(crate) async fn update_vfs_workspace(
        &self,
        params: VfsWorkspaceUpdateParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceUpdateResponse>, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACE_UPDATE, params).await
    }

    pub(crate) async fn delete_vfs_workspace(
        &self,
        params: VfsWorkspaceDeleteParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceDeleteResponse>, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACE_DELETE, params).await
    }

    pub(crate) async fn put_vfs_mount(
        &self,
        params: VfsMountPutParams,
    ) -> Result<AgentApiOutcome<VfsMountPutResponse>, AgentApiError> {
        self.request(METHOD_VFS_MOUNT_PUT, params).await
    }

    pub(crate) async fn delete_vfs_mount(
        &self,
        params: VfsMountDeleteParams,
    ) -> Result<AgentApiOutcome<VfsMountDeleteResponse>, AgentApiError> {
        self.request(METHOD_VFS_MOUNT_DELETE, params).await
    }

    pub(crate) async fn list_vfs_mounts(
        &self,
        params: VfsMountListParams,
    ) -> Result<AgentApiOutcome<VfsMountListResponse>, AgentApiError> {
        self.request(METHOD_VFS_MOUNT_LIST, params).await
    }

    pub(crate) async fn create_mcp_server(
        &self,
        params: McpServerCreateParams,
    ) -> Result<AgentApiOutcome<McpServerCreateResponse>, AgentApiError> {
        self.request(METHOD_MCP_SERVERS_CREATE, params).await
    }

    pub(crate) async fn list_mcp_servers(
        &self,
        params: McpServerListParams,
    ) -> Result<AgentApiOutcome<McpServerListResponse>, AgentApiError> {
        self.request(METHOD_MCP_SERVERS_LIST, params).await
    }

    pub(crate) async fn read_mcp_server(
        &self,
        params: McpServerReadParams,
    ) -> Result<AgentApiOutcome<McpServerReadResponse>, AgentApiError> {
        self.request(METHOD_MCP_SERVERS_READ, params).await
    }

    pub(crate) async fn delete_mcp_server(
        &self,
        params: McpServerDeleteParams,
    ) -> Result<AgentApiOutcome<McpServerDeleteResponse>, AgentApiError> {
        self.request(METHOD_MCP_SERVERS_DELETE, params).await
    }

    pub(crate) async fn import_auth_grant(
        &self,
        params: AuthGrantImportParams,
    ) -> Result<AgentApiOutcome<AuthGrantImportResponse>, AgentApiError> {
        self.request(METHOD_AUTH_GRANTS_IMPORT, params).await
    }

    pub(crate) async fn list_auth_grants(
        &self,
        params: AuthGrantListParams,
    ) -> Result<AgentApiOutcome<AuthGrantListResponse>, AgentApiError> {
        self.request(METHOD_AUTH_GRANTS_LIST, params).await
    }

    pub(crate) async fn read_auth_grant(
        &self,
        params: AuthGrantReadParams,
    ) -> Result<AgentApiOutcome<AuthGrantReadResponse>, AgentApiError> {
        self.request(METHOD_AUTH_GRANTS_READ, params).await
    }

    pub(crate) async fn revoke_auth_grant(
        &self,
        params: AuthGrantRevokeParams,
    ) -> Result<AgentApiOutcome<AuthGrantRevokeResponse>, AgentApiError> {
        self.request(METHOD_AUTH_GRANTS_REVOKE, params).await
    }

    pub(crate) async fn create_auth_client(
        &self,
        params: AuthClientCreateParams,
    ) -> Result<AgentApiOutcome<AuthClientCreateResponse>, AgentApiError> {
        self.request(METHOD_AUTH_CLIENTS_CREATE, params).await
    }

    pub(crate) async fn list_auth_clients(
        &self,
        params: AuthClientListParams,
    ) -> Result<AgentApiOutcome<AuthClientListResponse>, AgentApiError> {
        self.request(METHOD_AUTH_CLIENTS_LIST, params).await
    }

    pub(crate) async fn read_auth_client(
        &self,
        params: AuthClientReadParams,
    ) -> Result<AgentApiOutcome<AuthClientReadResponse>, AgentApiError> {
        self.request(METHOD_AUTH_CLIENTS_READ, params).await
    }

    pub(crate) async fn delete_auth_client(
        &self,
        params: AuthClientDeleteParams,
    ) -> Result<AgentApiOutcome<AuthClientDeleteResponse>, AgentApiError> {
        self.request(METHOD_AUTH_CLIENTS_DELETE, params).await
    }

    pub(crate) async fn start_auth_flow(
        &self,
        params: AuthFlowStartParams,
    ) -> Result<AgentApiOutcome<AuthFlowStartResponse>, AgentApiError> {
        self.request(METHOD_AUTH_FLOWS_START, params).await
    }

    pub(crate) async fn read_auth_flow_status(
        &self,
        params: AuthFlowStatusParams,
    ) -> Result<AgentApiOutcome<AuthFlowStatusResponse>, AgentApiError> {
        self.request(METHOD_AUTH_FLOWS_STATUS, params).await
    }

    pub(crate) async fn create_auth_provider(
        &self,
        params: AuthProviderCreateParams,
    ) -> Result<AgentApiOutcome<AuthProviderCreateResponse>, AgentApiError> {
        self.request(METHOD_AUTH_PROVIDERS_CREATE, params).await
    }

    pub(crate) async fn list_auth_providers(
        &self,
        params: AuthProviderListParams,
    ) -> Result<AgentApiOutcome<AuthProviderListResponse>, AgentApiError> {
        self.request(METHOD_AUTH_PROVIDERS_LIST, params).await
    }

    pub(crate) async fn read_auth_provider(
        &self,
        params: AuthProviderReadParams,
    ) -> Result<AgentApiOutcome<AuthProviderReadResponse>, AgentApiError> {
        self.request(METHOD_AUTH_PROVIDERS_READ, params).await
    }

    pub(crate) async fn delete_auth_provider(
        &self,
        params: AuthProviderDeleteParams,
    ) -> Result<AgentApiOutcome<AuthProviderDeleteResponse>, AgentApiError> {
        self.request(METHOD_AUTH_PROVIDERS_DELETE, params).await
    }

    pub(crate) async fn list_github_installations(
        &self,
        params: AuthGitHubInstallationListParams,
    ) -> Result<AgentApiOutcome<AuthGitHubInstallationListResponse>, AgentApiError> {
        self.request(METHOD_AUTH_GITHUB_INSTALLATIONS_LIST, params)
            .await
    }

    pub(crate) async fn grant_github_installation(
        &self,
        params: AuthGitHubInstallationGrantParams,
    ) -> Result<AgentApiOutcome<AuthGitHubInstallationGrantResponse>, AgentApiError> {
        self.request(METHOD_AUTH_GITHUB_INSTALLATIONS_GRANT, params)
            .await
    }

    pub(crate) async fn link_session_mcp(
        &self,
        params: SessionMcpLinkParams,
    ) -> Result<AgentApiOutcome<SessionMcpLinkResponse>, AgentApiError> {
        self.request(METHOD_SESSION_MCP_LINK, params).await
    }

    pub(crate) async fn unlink_session_mcp(
        &self,
        params: SessionMcpUnlinkParams,
    ) -> Result<AgentApiOutcome<SessionMcpUnlinkResponse>, AgentApiError> {
        self.request(METHOD_SESSION_MCP_UNLINK, params).await
    }

    pub(crate) async fn list_session_mcp(
        &self,
        params: SessionMcpListParams,
    ) -> Result<AgentApiOutcome<SessionMcpListResponse>, AgentApiError> {
        self.request(METHOD_SESSION_MCP_LIST, params).await
    }

    pub(crate) async fn list_session_environments(
        &self,
        params: SessionEnvironmentListParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentListResponse>, AgentApiError> {
        self.request(METHOD_SESSION_ENVIRONMENTS_LIST, params).await
    }

    pub(crate) async fn read_session_environment(
        &self,
        params: SessionEnvironmentReadParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentReadResponse>, AgentApiError> {
        self.request(METHOD_SESSION_ENVIRONMENTS_READ, params).await
    }

    pub(crate) async fn attach_session_environment(
        &self,
        params: SessionEnvironmentAttachParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentAttachResponse>, AgentApiError> {
        self.request(METHOD_SESSION_ENVIRONMENTS_ATTACH, params)
            .await
    }

    pub(crate) async fn activate_session_environment(
        &self,
        params: SessionEnvironmentActivateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentActivateResponse>, AgentApiError> {
        self.request(METHOD_SESSION_ENVIRONMENTS_ACTIVATE, params)
            .await
    }

    pub(crate) async fn deactivate_session_environment(
        &self,
        params: SessionEnvironmentDeactivateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentDeactivateResponse>, AgentApiError> {
        self.request(METHOD_SESSION_ENVIRONMENTS_DEACTIVATE, params)
            .await
    }

    pub(crate) async fn close_session_environment(
        &self,
        params: SessionEnvironmentCloseParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentCloseResponse>, AgentApiError> {
        self.request(METHOD_SESSION_ENVIRONMENTS_CLOSE, params)
            .await
    }

    async fn request<P, R>(
        &self,
        method: &str,
        params: P,
    ) -> Result<AgentApiOutcome<R>, AgentApiError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let id = RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed));
        let request = JsonRpcRequest {
            id,
            method: method.to_owned(),
            params: Some(serde_json::to_value(params).map_err(|error| {
                AgentApiError::invalid_request(format!("failed to encode API params: {error}"))
            })?),
        };
        let response = self
            .client
            .post(&self.endpoint)
            .json(&request)
            .send()
            .await
            .map_err(|error| AgentApiError::internal(format!("API request failed: {error}")))?
            .error_for_status()
            .map_err(|error| AgentApiError::internal(format!("API request failed: {error}")))?
            .json::<JsonRpcResponse>()
            .await
            .map_err(|error| AgentApiError::internal(format!("invalid API response: {error}")))?;
        if let Some(error) = response.error {
            return Err(agent_error_from_json_rpc(error));
        }
        let value = response
            .result
            .ok_or_else(|| AgentApiError::internal("JSON-RPC response missing result"))?;
        serde_json::from_value::<AgentApiOutcome<R>>(value)
            .map_err(|error| AgentApiError::internal(format!("invalid API result: {error}")))
    }
}

pub(crate) fn api_error(error: api::AgentApiError) -> anyhow::Error {
    anyhow::anyhow!("{error}")
}

fn agent_error_from_json_rpc(error: api::JsonRpcError) -> AgentApiError {
    if let Some(error) = error.data {
        return error;
    }
    let kind = match error.code {
        -32602 => AgentApiErrorKind::InvalidRequest,
        -32004 => AgentApiErrorKind::NotFound,
        -32009 => AgentApiErrorKind::Conflict,
        -32010 => AgentApiErrorKind::Rejected,
        _ => AgentApiErrorKind::Internal,
    };
    AgentApiError::new(kind, error.message)
}
