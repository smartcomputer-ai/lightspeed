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
    AuthProviderReadParams, AuthProviderReadResponse, BlobHasParams, BlobHasResponse,
    BlobPutParams, BlobPutResponse, BlobReadParams, BlobReadResponse, EnvironmentCloseParams,
    EnvironmentCloseResponse, EnvironmentCredentialBindParams, EnvironmentCredentialBindResponse,
    EnvironmentCredentialListParams, EnvironmentCredentialListResponse,
    EnvironmentCredentialUnbindParams, EnvironmentCredentialUnbindResponse,
    EnvironmentIdlePolicyPutParams, EnvironmentIdlePolicyPutResponse, EnvironmentListParams,
    EnvironmentListResponse, EnvironmentPowerPutParams, EnvironmentPowerPutResponse,
    EnvironmentProviderBindingListParams, EnvironmentProviderBindingListResponse,
    EnvironmentReadParams, EnvironmentReadResponse, JsonRpcRequest, JsonRpcResponse,
    METHOD_AUTH_CLIENTS_CREATE, METHOD_AUTH_CLIENTS_DELETE, METHOD_AUTH_CLIENTS_LIST,
    METHOD_AUTH_CLIENTS_READ, METHOD_AUTH_FLOWS_READ, METHOD_AUTH_FLOWS_START,
    METHOD_AUTH_GITHUB_INSTALLATIONS_GRANT, METHOD_AUTH_GITHUB_INSTALLATIONS_LIST,
    METHOD_AUTH_GRANTS_IMPORT, METHOD_AUTH_GRANTS_LIST, METHOD_AUTH_GRANTS_READ,
    METHOD_AUTH_GRANTS_REVOKE, METHOD_AUTH_PROVIDERS_CREATE, METHOD_AUTH_PROVIDERS_DELETE,
    METHOD_AUTH_PROVIDERS_LIST, METHOD_AUTH_PROVIDERS_READ, METHOD_BLOBS_HAS, METHOD_BLOBS_PUT,
    METHOD_BLOBS_READ, METHOD_ENVIRONMENTS_CLOSE, METHOD_ENVIRONMENTS_CREDENTIALS_BIND,
    METHOD_ENVIRONMENTS_CREDENTIALS_LIST, METHOD_ENVIRONMENTS_CREDENTIALS_UNBIND,
    METHOD_ENVIRONMENTS_IDLE_POLICY_PUT, METHOD_ENVIRONMENTS_LIST, METHOD_ENVIRONMENTS_POWER_PUT,
    METHOD_ENVIRONMENTS_PROVIDER_BINDINGS_LIST, METHOD_ENVIRONMENTS_READ,
    METHOD_MCP_SERVERS_DELETE, METHOD_MCP_SERVERS_LIST, METHOD_MCP_SERVERS_PUT,
    METHOD_MCP_SERVERS_READ, METHOD_PROFILES_DELETE, METHOD_PROFILES_LIST, METHOD_PROFILES_PUT,
    METHOD_PROFILES_READ, METHOD_SESSION_CONFIG_PUT, METHOD_SESSION_ENVIRONMENTS_ACTIVATE,
    METHOD_SESSION_ENVIRONMENTS_DEACTIVATE, METHOD_SESSION_EVENTS_READ, METHOD_SESSION_LIST,
    METHOD_SESSION_PROFILES_APPLY, METHOD_SESSION_READ, METHOD_SESSION_RUNS_APPROVALS_DECIDE,
    METHOD_SESSION_RUNS_CANCEL, METHOD_SESSION_RUNS_START, METHOD_SESSION_RUNS_STEER,
    METHOD_SESSION_SKILLS_LIST, METHOD_SESSION_START, METHOD_VFS_SNAPSHOTS_COMMIT,
    METHOD_VFS_SNAPSHOTS_READ, METHOD_VFS_WORKSPACES_CREATE, METHOD_VFS_WORKSPACES_DELETE,
    METHOD_VFS_WORKSPACES_LIST, METHOD_VFS_WORKSPACES_READ, METHOD_VFS_WORKSPACES_UPDATE,
    McpServerDeleteParams, McpServerDeleteResponse, McpServerListParams, McpServerListResponse,
    McpServerPutParams, McpServerPutResponse, McpServerReadParams, McpServerReadResponse,
    ProfileApplyParams, ProfileApplyResponse, ProfileDeleteParams, ProfileDeleteResponse,
    ProfileListParams, ProfileListResponse, ProfilePutParams, ProfilePutResponse,
    ProfileReadParams, ProfileReadResponse, RequestId, RunApprovalsDecideParams,
    RunApprovalsDecideResponse, RunCancelParams, RunCancelResponse, RunStartParams,
    RunStartResponse, RunSteerParams, RunSteerResponse, SessionConfigPutParams,
    SessionConfigPutResponse, SessionEnvironmentActivateParams, SessionEnvironmentActivateResponse,
    SessionEnvironmentDeactivateParams, SessionEnvironmentDeactivateResponse,
    SessionEventsReadParams, SessionEventsReadResponse, SessionListParams, SessionListResponse,
    SessionReadParams, SessionReadResponse, SessionStartParams, SessionStartResponse,
    SkillListParams, SkillListResponse, VfsSnapshotCommitParams, VfsSnapshotCommitResponse,
    VfsSnapshotReadParams, VfsSnapshotReadResponse, VfsWorkspaceCreateParams,
    VfsWorkspaceCreateResponse, VfsWorkspaceDeleteParams, VfsWorkspaceDeleteResponse,
    VfsWorkspaceListParams, VfsWorkspaceListResponse, VfsWorkspaceReadParams,
    VfsWorkspaceReadResponse, VfsWorkspaceUpdateParams, VfsWorkspaceUpdateResponse,
};
use serde::{Serialize, de::DeserializeOwned};

/// Gateway auth headers from the environment, applied to every request:
/// `LIGHTSPEED_API_KEY` becomes `Authorization: Bearer …` (api-key
/// deployments) and `LIGHTSPEED_UNIVERSE` becomes `x-lightspeed-universe`
/// (trusted-header deployments behind a proxy that forwards it). Both are
/// optional; a plain `single`-mode gateway needs neither.
fn auth_headers_from_env() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(api_key) = std::env::var("LIGHTSPEED_API_KEY") {
        let api_key = api_key.trim();
        if !api_key.is_empty()
            && let Ok(mut value) =
                reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))
        {
            value.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
    }
    if let Ok(universe) = std::env::var("LIGHTSPEED_UNIVERSE") {
        let universe = universe.trim();
        if !universe.is_empty()
            && let Ok(value) = reqwest::header::HeaderValue::from_str(universe)
        {
            headers.insert("x-lightspeed-universe", value);
        }
    }
    headers
}

pub(crate) struct HttpAgentApi {
    endpoint: String,
    client: reqwest::Client,
    next_id: AtomicU64,
}

impl HttpAgentApi {
    pub(crate) fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: reqwest::Client::builder()
                .default_headers(auth_headers_from_env())
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
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
                    run_limit: None,
                })
                .await
                .map(|outcome| {
                    AgentApiOutcome::with_notifications(
                        SessionStartResponse {
                            session: api::SessionMutationView {
                                id: outcome.result.session.id,
                                status: outcome.result.session.status,
                                head_cursor: None,
                                config_revision: outcome.result.session.config_revision,
                                context_revision: outcome.result.session.active_context.revision,
                            },
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

    pub(crate) async fn put_profile(
        &self,
        params: ProfilePutParams,
    ) -> Result<AgentApiOutcome<ProfilePutResponse>, AgentApiError> {
        self.request(METHOD_PROFILES_PUT, params).await
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
        self.request(METHOD_SESSION_PROFILES_APPLY, params).await
    }

    pub(crate) async fn read_session(
        &self,
        params: SessionReadParams,
    ) -> Result<AgentApiOutcome<SessionReadResponse>, AgentApiError> {
        self.request(METHOD_SESSION_READ, params).await
    }

    pub(crate) async fn list_sessions(
        &self,
        params: SessionListParams,
    ) -> Result<AgentApiOutcome<SessionListResponse>, AgentApiError> {
        self.request(METHOD_SESSION_LIST, params).await
    }

    pub(crate) async fn put_session_metadata(
        &self,
        params: api::SessionMetadataPutParams,
    ) -> Result<AgentApiOutcome<api::SessionMetadataPutResponse>, AgentApiError> {
        self.request(api::METHOD_SESSION_METADATA_PUT, params).await
    }

    pub(crate) async fn put_session_retention(
        &self,
        params: api::SessionRetentionPutParams,
    ) -> Result<AgentApiOutcome<api::SessionRetentionPutResponse>, AgentApiError> {
        self.request(api::METHOD_SESSION_RETENTION_PUT, params)
            .await
    }

    pub(crate) async fn close_session(
        &self,
        params: api::SessionCloseParams,
    ) -> Result<AgentApiOutcome<api::SessionCloseResponse>, AgentApiError> {
        self.request(api::METHOD_SESSION_CLOSE, params).await
    }

    pub(crate) async fn delete_session(
        &self,
        params: api::SessionDeleteParams,
    ) -> Result<AgentApiOutcome<api::SessionDeleteResponse>, AgentApiError> {
        self.request(api::METHOD_SESSION_DELETE, params).await
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
        self.request(METHOD_SESSION_RUNS_START, params).await
    }

    pub(crate) async fn cancel_run(
        &self,
        params: RunCancelParams,
    ) -> Result<AgentApiOutcome<RunCancelResponse>, AgentApiError> {
        self.request(METHOD_SESSION_RUNS_CANCEL, params).await
    }

    pub(crate) async fn decide_run_approvals(
        &self,
        params: RunApprovalsDecideParams,
    ) -> Result<AgentApiOutcome<RunApprovalsDecideResponse>, AgentApiError> {
        self.request(METHOD_SESSION_RUNS_APPROVALS_DECIDE, params)
            .await
    }

    pub(crate) async fn steer_run(
        &self,
        params: RunSteerParams,
    ) -> Result<AgentApiOutcome<RunSteerResponse>, AgentApiError> {
        self.request(METHOD_SESSION_RUNS_STEER, params).await
    }

    pub(crate) async fn list_skills(
        &self,
        params: SkillListParams,
    ) -> Result<AgentApiOutcome<SkillListResponse>, AgentApiError> {
        self.request(METHOD_SESSION_SKILLS_LIST, params).await
    }

    pub(crate) async fn put_blobs(
        &self,
        params: BlobPutParams,
    ) -> Result<AgentApiOutcome<BlobPutResponse>, AgentApiError> {
        self.request(METHOD_BLOBS_PUT, params).await
    }

    pub(crate) async fn has_blobs(
        &self,
        params: BlobHasParams,
    ) -> Result<AgentApiOutcome<BlobHasResponse>, AgentApiError> {
        self.request(METHOD_BLOBS_HAS, params).await
    }

    pub(crate) async fn read_blob(
        &self,
        params: BlobReadParams,
    ) -> Result<AgentApiOutcome<BlobReadResponse>, AgentApiError> {
        self.request(METHOD_BLOBS_READ, params).await
    }

    pub(crate) async fn commit_vfs_snapshot(
        &self,
        params: VfsSnapshotCommitParams,
    ) -> Result<AgentApiOutcome<VfsSnapshotCommitResponse>, AgentApiError> {
        self.request(METHOD_VFS_SNAPSHOTS_COMMIT, params).await
    }

    pub(crate) async fn read_vfs_snapshot(
        &self,
        params: VfsSnapshotReadParams,
    ) -> Result<AgentApiOutcome<VfsSnapshotReadResponse>, AgentApiError> {
        self.request(METHOD_VFS_SNAPSHOTS_READ, params).await
    }

    pub(crate) async fn create_vfs_workspace(
        &self,
        params: VfsWorkspaceCreateParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceCreateResponse>, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACES_CREATE, params).await
    }

    pub(crate) async fn read_vfs_workspace(
        &self,
        params: VfsWorkspaceReadParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceReadResponse>, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACES_READ, params).await
    }

    pub(crate) async fn list_vfs_workspaces(
        &self,
        params: VfsWorkspaceListParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceListResponse>, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACES_LIST, params).await
    }

    pub(crate) async fn update_vfs_workspace(
        &self,
        params: VfsWorkspaceUpdateParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceUpdateResponse>, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACES_UPDATE, params).await
    }

    pub(crate) async fn delete_vfs_workspace(
        &self,
        params: VfsWorkspaceDeleteParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceDeleteResponse>, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACES_DELETE, params).await
    }

    pub(crate) async fn put_mcp_server(
        &self,
        params: McpServerPutParams,
    ) -> Result<AgentApiOutcome<McpServerPutResponse>, AgentApiError> {
        self.request(METHOD_MCP_SERVERS_PUT, params).await
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
        self.request(METHOD_AUTH_FLOWS_READ, params).await
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

    pub(crate) async fn put_session_config(
        &self,
        params: SessionConfigPutParams,
    ) -> Result<AgentApiOutcome<SessionConfigPutResponse>, AgentApiError> {
        self.request(METHOD_SESSION_CONFIG_PUT, params).await
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

    pub(crate) async fn bind_environment_credential(
        &self,
        params: EnvironmentCredentialBindParams,
    ) -> Result<AgentApiOutcome<EnvironmentCredentialBindResponse>, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_CREDENTIALS_BIND, params)
            .await
    }

    pub(crate) async fn list_environment_credentials(
        &self,
        params: EnvironmentCredentialListParams,
    ) -> Result<AgentApiOutcome<EnvironmentCredentialListResponse>, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_CREDENTIALS_LIST, params)
            .await
    }

    pub(crate) async fn unbind_environment_credential(
        &self,
        params: EnvironmentCredentialUnbindParams,
    ) -> Result<AgentApiOutcome<EnvironmentCredentialUnbindResponse>, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_CREDENTIALS_UNBIND, params)
            .await
    }

    pub(crate) async fn list_environment_provider_bindings(
        &self,
        params: EnvironmentProviderBindingListParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderBindingListResponse>, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_PROVIDER_BINDINGS_LIST, params)
            .await
    }

    pub(crate) async fn list_environments(
        &self,
        params: EnvironmentListParams,
    ) -> Result<AgentApiOutcome<EnvironmentListResponse>, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_LIST, params).await
    }

    pub(crate) async fn read_environment(
        &self,
        params: EnvironmentReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentReadResponse>, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_READ, params).await
    }

    pub(crate) async fn close_environment(
        &self,
        params: EnvironmentCloseParams,
    ) -> Result<AgentApiOutcome<EnvironmentCloseResponse>, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_CLOSE, params).await
    }

    pub(crate) async fn put_environment_power(
        &self,
        params: EnvironmentPowerPutParams,
    ) -> Result<AgentApiOutcome<EnvironmentPowerPutResponse>, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_POWER_PUT, params).await
    }

    pub(crate) async fn put_environment_idle_policy(
        &self,
        params: EnvironmentIdlePolicyPutParams,
    ) -> Result<AgentApiOutcome<EnvironmentIdlePolicyPutResponse>, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_IDLE_POLICY_PUT, params)
            .await
    }

    pub(crate) async fn create_environment_registration_key(
        &self,
        params: api::EnvironmentRegistrationKeyCreateParams,
    ) -> Result<AgentApiOutcome<api::EnvironmentRegistrationKeyCreateResponse>, AgentApiError> {
        self.request(api::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_CREATE, params)
            .await
    }

    pub(crate) async fn read_environment_registration_key(
        &self,
        params: api::EnvironmentRegistrationKeyReadParams,
    ) -> Result<AgentApiOutcome<api::EnvironmentRegistrationKeyReadResponse>, AgentApiError> {
        self.request(api::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_READ, params)
            .await
    }

    pub(crate) async fn list_environment_registration_keys(
        &self,
        params: api::EnvironmentRegistrationKeyListParams,
    ) -> Result<AgentApiOutcome<api::EnvironmentRegistrationKeyListResponse>, AgentApiError> {
        self.request(api::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_LIST, params)
            .await
    }

    pub(crate) async fn revoke_environment_registration_key(
        &self,
        params: api::EnvironmentRegistrationKeyRevokeParams,
    ) -> Result<AgentApiOutcome<api::EnvironmentRegistrationKeyRevokeResponse>, AgentApiError> {
        self.request(api::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_REVOKE, params)
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
