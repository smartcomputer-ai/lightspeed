use api::{
    AgentApiError, AgentApiErrorKind, BlobHasParams, BlobHasResponse, BlobPutParams,
    BlobPutResponse, BlobReadParams, CredentialClientCreateParams, CredentialClientCreateResponse,
    CredentialClientDeleteParams, CredentialClientDeleteResponse, CredentialClientListParams,
    CredentialClientListResponse, CredentialClientReadParams, CredentialClientReadResponse,
    CredentialFlowStartParams, CredentialFlowStartResponse, CredentialFlowStatusParams,
    CredentialFlowStatusResponse, CredentialGitHubInstallationGrantParams,
    CredentialGitHubInstallationGrantResponse, CredentialGitHubInstallationListParams,
    CredentialGitHubInstallationListResponse, CredentialGrantListParams,
    CredentialGrantListResponse, CredentialGrantReadParams, CredentialGrantReadResponse,
    CredentialGrantRevokeParams, CredentialGrantRevokeResponse, CredentialProviderCreateParams,
    CredentialProviderCreateResponse, CredentialProviderDeleteParams,
    CredentialProviderDeleteResponse, CredentialProviderListParams, CredentialProviderListResponse,
    CredentialProviderReadParams, CredentialProviderReadResponse, EnvironmentCloseParams,
    EnvironmentCloseResponse, EnvironmentCredentialBindParams, EnvironmentCredentialBindResponse,
    EnvironmentCredentialListParams, EnvironmentCredentialListResponse,
    EnvironmentCredentialUnbindParams, EnvironmentCredentialUnbindResponse, EnvironmentListParams,
    EnvironmentListResponse, EnvironmentPowerPutParams, EnvironmentPowerPutResponse,
    EnvironmentProviderBindingListParams, EnvironmentProviderBindingListResponse,
    EnvironmentReadParams, EnvironmentReadResponse, METHOD_BLOBS_HAS, METHOD_BLOBS_PUT,
    METHOD_BLOBS_READ, METHOD_CREDENTIALS_CLIENTS_CREATE, METHOD_CREDENTIALS_CLIENTS_DELETE,
    METHOD_CREDENTIALS_CLIENTS_LIST, METHOD_CREDENTIALS_CLIENTS_READ,
    METHOD_CREDENTIALS_FLOWS_READ, METHOD_CREDENTIALS_FLOWS_START,
    METHOD_CREDENTIALS_GITHUB_INSTALLATIONS_GRANT, METHOD_CREDENTIALS_GITHUB_INSTALLATIONS_LIST,
    METHOD_CREDENTIALS_GRANTS_LIST, METHOD_CREDENTIALS_GRANTS_READ,
    METHOD_CREDENTIALS_GRANTS_REVOKE, METHOD_CREDENTIALS_PROVIDERS_CREATE,
    METHOD_CREDENTIALS_PROVIDERS_DELETE, METHOD_CREDENTIALS_PROVIDERS_LIST,
    METHOD_CREDENTIALS_PROVIDERS_READ, METHOD_ENVIRONMENTS_CLOSE,
    METHOD_ENVIRONMENTS_CREDENTIALS_BIND, METHOD_ENVIRONMENTS_CREDENTIALS_LIST,
    METHOD_ENVIRONMENTS_CREDENTIALS_UNBIND, METHOD_ENVIRONMENTS_LIST,
    METHOD_ENVIRONMENTS_POWER_PUT, METHOD_ENVIRONMENTS_PROVIDER_BINDINGS_LIST,
    METHOD_ENVIRONMENTS_READ, METHOD_MCP_SERVERS_DELETE, METHOD_MCP_SERVERS_LIST,
    METHOD_MCP_SERVERS_PUT, METHOD_MCP_SERVERS_READ, METHOD_MODELS_LIST, METHOD_PROFILES_DELETE,
    METHOD_PROFILES_LIST, METHOD_PROFILES_PUT, METHOD_PROFILES_READ, METHOD_SESSION_CONFIG_PUT,
    METHOD_SESSION_EVENTS_READ, METHOD_SESSION_LIST, METHOD_SESSION_PROFILES_APPLY,
    METHOD_SESSION_READ, METHOD_SESSION_RUNS_APPROVALS_DECIDE, METHOD_SESSION_RUNS_CANCEL,
    METHOD_SESSION_RUNS_READ, METHOD_SESSION_RUNS_START, METHOD_SESSION_RUNS_STEER,
    METHOD_SESSION_SKILLS_LIST, METHOD_SESSION_START, METHOD_VFS_SNAPSHOTS_COMMIT,
    METHOD_VFS_SNAPSHOTS_READ, METHOD_VFS_WORKSPACES_CREATE, METHOD_VFS_WORKSPACES_DELETE,
    METHOD_VFS_WORKSPACES_LIST, METHOD_VFS_WORKSPACES_READ, METHOD_VFS_WORKSPACES_UPDATE,
    McpServerDeleteParams, McpServerDeleteResponse, McpServerListParams, McpServerListResponse,
    McpServerPutParams, McpServerPutResponse, McpServerReadParams, McpServerReadResponse,
    ModelListParams, ModelListResponse, ProfileApplyParams, ProfileApplyResponse,
    ProfileDeleteParams, ProfileDeleteResponse, ProfileListParams, ProfileListResponse,
    ProfilePutParams, ProfilePutResponse, ProfileReadParams, ProfileReadResponse,
    RunApprovalsDecideParams, RunApprovalsDecideResponse, RunCancelParams, RunCancelResponse,
    RunReadParams, RunReadResponse, RunStartParams, RunStartResponse, RunSteerParams,
    RunSteerResponse, SessionConfigPutParams, SessionConfigPutResponse, SessionEventsReadParams,
    SessionEventsReadResponse, SessionListParams, SessionListResponse, SessionReadParams,
    SessionReadResponse, SessionStartParams, SessionStartResponse, SkillListParams,
    SkillListResponse, VfsSnapshotCommitParams, VfsSnapshotCommitResponse, VfsSnapshotReadParams,
    VfsSnapshotReadResponse, VfsWorkspaceCreateParams, VfsWorkspaceCreateResponse,
    VfsWorkspaceDeleteParams, VfsWorkspaceDeleteResponse, VfsWorkspaceListParams,
    VfsWorkspaceListResponse, VfsWorkspaceReadParams, VfsWorkspaceReadResponse,
    VfsWorkspaceUpdateParams, VfsWorkspaceUpdateResponse,
};
use serde::{Serialize, de::DeserializeOwned};

pub(crate) struct HttpAgentApi {
    client: reqwest::Client,
    connection: crate::connection::ResolvedConnection,
}

impl HttpAgentApi {
    /// Build a command client from the connection already resolved by the CLI.
    pub(crate) fn from_active() -> Self {
        Self::with_connection(crate::connection::active().clone())
    }

    pub(crate) fn with_connection(connection: crate::connection::ResolvedConnection) -> Self {
        Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("HTTP client"),
            connection,
        }
    }

    /// The connection this client sends requests over.
    pub(crate) fn connection(&self) -> &crate::connection::ResolvedConnection {
        &self.connection
    }

    pub(crate) async fn open_or_start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<SessionStartResponse, AgentApiError> {
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
                .map(|response| SessionStartResponse {
                    session: api::SessionMutationView {
                        id: response.session.id,
                        lifecycle_status: response.session.lifecycle_status,
                        activity: response.session.activity,
                        head_cursor: None,
                        config_revision: response.session.config_revision,
                        context_revision: response.session.active_context.revision,
                    },
                })
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<SessionStartResponse, AgentApiError> {
        self.request(METHOD_SESSION_START, params).await
    }

    pub(crate) async fn read_profile(
        &self,
        params: ProfileReadParams,
    ) -> Result<ProfileReadResponse, AgentApiError> {
        self.request(METHOD_PROFILES_READ, params).await
    }

    pub(crate) async fn list_profiles(
        &self,
        params: ProfileListParams,
    ) -> Result<ProfileListResponse, AgentApiError> {
        self.request(METHOD_PROFILES_LIST, params).await
    }

    pub(crate) async fn put_profile(
        &self,
        params: ProfilePutParams,
    ) -> Result<ProfilePutResponse, AgentApiError> {
        self.request(METHOD_PROFILES_PUT, params).await
    }

    pub(crate) async fn delete_profile(
        &self,
        params: ProfileDeleteParams,
    ) -> Result<ProfileDeleteResponse, AgentApiError> {
        self.request(METHOD_PROFILES_DELETE, params).await
    }

    pub(crate) async fn apply_profile(
        &self,
        params: ProfileApplyParams,
    ) -> Result<ProfileApplyResponse, AgentApiError> {
        self.request(METHOD_SESSION_PROFILES_APPLY, params).await
    }

    pub(crate) async fn read_session(
        &self,
        params: SessionReadParams,
    ) -> Result<SessionReadResponse, AgentApiError> {
        self.request(METHOD_SESSION_READ, params).await
    }

    pub(crate) async fn replace_context(
        &self,
        params: api::ContextReplaceParams,
    ) -> Result<api::ContextReplaceResponse, AgentApiError> {
        self.request(api::METHOD_SESSION_CONTEXT_REPLACE, params)
            .await
    }

    pub(crate) async fn read_run(
        &self,
        params: RunReadParams,
    ) -> Result<RunReadResponse, AgentApiError> {
        self.request(METHOD_SESSION_RUNS_READ, params).await
    }

    pub(crate) async fn list_sessions(
        &self,
        params: SessionListParams,
    ) -> Result<SessionListResponse, AgentApiError> {
        self.request(METHOD_SESSION_LIST, params).await
    }

    pub(crate) async fn put_session_metadata(
        &self,
        params: api::SessionMetadataPutParams,
    ) -> Result<api::SessionMetadataPutResponse, AgentApiError> {
        self.request(api::METHOD_SESSION_METADATA_PUT, params).await
    }

    pub(crate) async fn put_session_retention(
        &self,
        params: api::SessionRetentionPutParams,
    ) -> Result<api::SessionRetentionPutResponse, AgentApiError> {
        self.request(api::METHOD_SESSION_RETENTION_PUT, params)
            .await
    }

    pub(crate) async fn restart_session(
        &self,
        params: api::SessionRestartParams,
    ) -> Result<api::SessionRestartResponse, AgentApiError> {
        self.request(api::METHOD_SESSION_RESTART, params).await
    }

    pub(crate) async fn close_session(
        &self,
        params: api::SessionCloseParams,
    ) -> Result<api::SessionCloseResponse, AgentApiError> {
        self.request(api::METHOD_SESSION_CLOSE, params).await
    }

    pub(crate) async fn delete_session(
        &self,
        params: api::SessionDeleteParams,
    ) -> Result<api::SessionDeleteResponse, AgentApiError> {
        self.request(api::METHOD_SESSION_DELETE, params).await
    }

    pub(crate) async fn read_session_events(
        &self,
        params: SessionEventsReadParams,
    ) -> Result<SessionEventsReadResponse, AgentApiError> {
        self.request(METHOD_SESSION_EVENTS_READ, params).await
    }

    pub(crate) async fn start_run(
        &self,
        params: RunStartParams,
    ) -> Result<RunStartResponse, AgentApiError> {
        self.request(METHOD_SESSION_RUNS_START, params).await
    }

    pub(crate) async fn cancel_run(
        &self,
        params: RunCancelParams,
    ) -> Result<RunCancelResponse, AgentApiError> {
        self.request(METHOD_SESSION_RUNS_CANCEL, params).await
    }

    pub(crate) async fn decide_run_approvals(
        &self,
        params: RunApprovalsDecideParams,
    ) -> Result<RunApprovalsDecideResponse, AgentApiError> {
        self.request(METHOD_SESSION_RUNS_APPROVALS_DECIDE, params)
            .await
    }

    pub(crate) async fn steer_run(
        &self,
        params: RunSteerParams,
    ) -> Result<RunSteerResponse, AgentApiError> {
        self.request(METHOD_SESSION_RUNS_STEER, params).await
    }

    pub(crate) async fn list_skills(
        &self,
        params: SkillListParams,
    ) -> Result<SkillListResponse, AgentApiError> {
        self.request(METHOD_SESSION_SKILLS_LIST, params).await
    }

    pub(crate) async fn list_models(
        &self,
        params: ModelListParams,
    ) -> Result<ModelListResponse, AgentApiError> {
        self.request(METHOD_MODELS_LIST, params).await
    }

    pub(crate) async fn put_blobs(
        &self,
        params: BlobPutParams,
    ) -> Result<BlobPutResponse, AgentApiError> {
        self.request(METHOD_BLOBS_PUT, params).await
    }

    pub(crate) async fn has_blobs(
        &self,
        params: BlobHasParams,
    ) -> Result<BlobHasResponse, AgentApiError> {
        self.request(METHOD_BLOBS_HAS, params).await
    }

    pub(crate) async fn read_blob(&self, params: BlobReadParams) -> Result<Vec<u8>, AgentApiError> {
        let response = self
            .send(
                METHOD_BLOBS_READ,
                serde_json::to_value(params)
                    .map_err(|error| AgentApiError::invalid_request(error.to_string()))?,
            )
            .await
            .map_err(|error| match error.downcast::<AgentApiError>() {
                Ok(error) => error,
                Err(error) => AgentApiError::internal(error.to_string()),
            })?;
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| AgentApiError::internal(error.to_string()))
    }

    pub(crate) async fn commit_vfs_snapshot(
        &self,
        params: VfsSnapshotCommitParams,
    ) -> Result<VfsSnapshotCommitResponse, AgentApiError> {
        self.request(METHOD_VFS_SNAPSHOTS_COMMIT, params).await
    }

    pub(crate) async fn read_vfs_snapshot(
        &self,
        params: VfsSnapshotReadParams,
    ) -> Result<VfsSnapshotReadResponse, AgentApiError> {
        self.request(METHOD_VFS_SNAPSHOTS_READ, params).await
    }

    pub(crate) async fn create_vfs_workspace(
        &self,
        params: VfsWorkspaceCreateParams,
    ) -> Result<VfsWorkspaceCreateResponse, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACES_CREATE, params).await
    }

    pub(crate) async fn read_vfs_workspace(
        &self,
        params: VfsWorkspaceReadParams,
    ) -> Result<VfsWorkspaceReadResponse, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACES_READ, params).await
    }

    pub(crate) async fn list_vfs_workspaces(
        &self,
        params: VfsWorkspaceListParams,
    ) -> Result<VfsWorkspaceListResponse, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACES_LIST, params).await
    }

    pub(crate) async fn update_vfs_workspace(
        &self,
        params: VfsWorkspaceUpdateParams,
    ) -> Result<VfsWorkspaceUpdateResponse, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACES_UPDATE, params).await
    }

    pub(crate) async fn delete_vfs_workspace(
        &self,
        params: VfsWorkspaceDeleteParams,
    ) -> Result<VfsWorkspaceDeleteResponse, AgentApiError> {
        self.request(METHOD_VFS_WORKSPACES_DELETE, params).await
    }

    pub(crate) async fn put_mcp_server(
        &self,
        params: McpServerPutParams,
    ) -> Result<McpServerPutResponse, AgentApiError> {
        self.request(METHOD_MCP_SERVERS_PUT, params).await
    }

    pub(crate) async fn list_mcp_servers(
        &self,
        params: McpServerListParams,
    ) -> Result<McpServerListResponse, AgentApiError> {
        self.request(METHOD_MCP_SERVERS_LIST, params).await
    }

    pub(crate) async fn read_mcp_server(
        &self,
        params: McpServerReadParams,
    ) -> Result<McpServerReadResponse, AgentApiError> {
        self.request(METHOD_MCP_SERVERS_READ, params).await
    }

    pub(crate) async fn delete_mcp_server(
        &self,
        params: McpServerDeleteParams,
    ) -> Result<McpServerDeleteResponse, AgentApiError> {
        self.request(METHOD_MCP_SERVERS_DELETE, params).await
    }

    pub(crate) async fn list_credential_grants(
        &self,
        params: CredentialGrantListParams,
    ) -> Result<CredentialGrantListResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_GRANTS_LIST, params).await
    }

    pub(crate) async fn read_credential_grant(
        &self,
        params: CredentialGrantReadParams,
    ) -> Result<CredentialGrantReadResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_GRANTS_READ, params).await
    }

    pub(crate) async fn put_credential_grant(
        &self,
        params: api::CredentialGrantPutParams,
    ) -> Result<api::CredentialGrantPutResponse, AgentApiError> {
        self.request(api::METHOD_CREDENTIALS_GRANTS_PUT, params)
            .await
    }

    pub(crate) async fn revoke_credential_grant(
        &self,
        params: CredentialGrantRevokeParams,
    ) -> Result<CredentialGrantRevokeResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_GRANTS_REVOKE, params).await
    }

    pub(crate) async fn create_credential_client(
        &self,
        params: CredentialClientCreateParams,
    ) -> Result<CredentialClientCreateResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_CLIENTS_CREATE, params)
            .await
    }

    pub(crate) async fn list_credential_clients(
        &self,
        params: CredentialClientListParams,
    ) -> Result<CredentialClientListResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_CLIENTS_LIST, params).await
    }

    pub(crate) async fn read_credential_client(
        &self,
        params: CredentialClientReadParams,
    ) -> Result<CredentialClientReadResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_CLIENTS_READ, params).await
    }

    pub(crate) async fn delete_credential_client(
        &self,
        params: CredentialClientDeleteParams,
    ) -> Result<CredentialClientDeleteResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_CLIENTS_DELETE, params)
            .await
    }

    pub(crate) async fn start_credential_flow(
        &self,
        params: CredentialFlowStartParams,
    ) -> Result<CredentialFlowStartResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_FLOWS_START, params).await
    }

    pub(crate) async fn read_credential_flow_status(
        &self,
        params: CredentialFlowStatusParams,
    ) -> Result<CredentialFlowStatusResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_FLOWS_READ, params).await
    }

    pub(crate) async fn create_credential_provider(
        &self,
        params: CredentialProviderCreateParams,
    ) -> Result<CredentialProviderCreateResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_PROVIDERS_CREATE, params)
            .await
    }

    pub(crate) async fn list_credential_providers(
        &self,
        params: CredentialProviderListParams,
    ) -> Result<CredentialProviderListResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_PROVIDERS_LIST, params)
            .await
    }

    pub(crate) async fn read_credential_provider(
        &self,
        params: CredentialProviderReadParams,
    ) -> Result<CredentialProviderReadResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_PROVIDERS_READ, params)
            .await
    }

    pub(crate) async fn delete_credential_provider(
        &self,
        params: CredentialProviderDeleteParams,
    ) -> Result<CredentialProviderDeleteResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_PROVIDERS_DELETE, params)
            .await
    }

    pub(crate) async fn list_github_installations(
        &self,
        params: CredentialGitHubInstallationListParams,
    ) -> Result<CredentialGitHubInstallationListResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_GITHUB_INSTALLATIONS_LIST, params)
            .await
    }

    pub(crate) async fn grant_github_installation(
        &self,
        params: CredentialGitHubInstallationGrantParams,
    ) -> Result<CredentialGitHubInstallationGrantResponse, AgentApiError> {
        self.request(METHOD_CREDENTIALS_GITHUB_INSTALLATIONS_GRANT, params)
            .await
    }

    pub(crate) async fn put_session_config(
        &self,
        params: SessionConfigPutParams,
    ) -> Result<SessionConfigPutResponse, AgentApiError> {
        self.request(METHOD_SESSION_CONFIG_PUT, params).await
    }

    pub(crate) async fn bind_environment_credential(
        &self,
        params: EnvironmentCredentialBindParams,
    ) -> Result<EnvironmentCredentialBindResponse, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_CREDENTIALS_BIND, params)
            .await
    }

    pub(crate) async fn list_environment_credentials(
        &self,
        params: EnvironmentCredentialListParams,
    ) -> Result<EnvironmentCredentialListResponse, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_CREDENTIALS_LIST, params)
            .await
    }

    pub(crate) async fn unbind_environment_credential(
        &self,
        params: EnvironmentCredentialUnbindParams,
    ) -> Result<EnvironmentCredentialUnbindResponse, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_CREDENTIALS_UNBIND, params)
            .await
    }

    pub(crate) async fn list_environment_provider_bindings(
        &self,
        params: EnvironmentProviderBindingListParams,
    ) -> Result<EnvironmentProviderBindingListResponse, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_PROVIDER_BINDINGS_LIST, params)
            .await
    }

    pub(crate) async fn list_environments(
        &self,
        params: EnvironmentListParams,
    ) -> Result<EnvironmentListResponse, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_LIST, params).await
    }

    pub(crate) async fn read_environment(
        &self,
        params: EnvironmentReadParams,
    ) -> Result<EnvironmentReadResponse, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_READ, params).await
    }

    pub(crate) async fn close_environment(
        &self,
        params: EnvironmentCloseParams,
    ) -> Result<EnvironmentCloseResponse, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_CLOSE, params).await
    }

    pub(crate) async fn put_environment_power(
        &self,
        params: EnvironmentPowerPutParams,
    ) -> Result<EnvironmentPowerPutResponse, AgentApiError> {
        self.request(METHOD_ENVIRONMENTS_POWER_PUT, params).await
    }

    pub(crate) async fn update_environment(
        &self,
        params: api::EnvironmentUpdateParams,
    ) -> Result<api::EnvironmentUpdateResponse, AgentApiError> {
        self.request(api::METHOD_ENVIRONMENTS_UPDATE, params).await
    }

    pub(crate) async fn create_environment_registration_key(
        &self,
        params: api::EnvironmentRegistrationKeyCreateParams,
    ) -> Result<api::EnvironmentRegistrationKeyCreateResponse, AgentApiError> {
        self.request(api::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_CREATE, params)
            .await
    }

    pub(crate) async fn read_environment_registration_key(
        &self,
        params: api::EnvironmentRegistrationKeyReadParams,
    ) -> Result<api::EnvironmentRegistrationKeyReadResponse, AgentApiError> {
        self.request(api::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_READ, params)
            .await
    }

    pub(crate) async fn list_environment_registration_keys(
        &self,
        params: api::EnvironmentRegistrationKeyListParams,
    ) -> Result<api::EnvironmentRegistrationKeyListResponse, AgentApiError> {
        self.request(api::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_LIST, params)
            .await
    }

    pub(crate) async fn update_environment_registration_key(
        &self,
        params: api::EnvironmentRegistrationKeyUpdateParams,
    ) -> Result<api::EnvironmentRegistrationKeyUpdateResponse, AgentApiError> {
        self.request(api::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_UPDATE, params)
            .await
    }

    pub(crate) async fn put_environment_registration_key(
        &self,
        params: api::EnvironmentRegistrationKeyPutParams,
    ) -> Result<api::EnvironmentRegistrationKeyPutResponse, AgentApiError> {
        self.request(api::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_PUT, params)
            .await
    }

    pub(crate) async fn rotate_environment_registration_key(
        &self,
        params: api::EnvironmentRegistrationKeyRotateParams,
    ) -> Result<api::EnvironmentRegistrationKeyRotateResponse, AgentApiError> {
        self.request(api::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_ROTATE, params)
            .await
    }

    pub(crate) async fn revoke_environment_registration_key(
        &self,
        params: api::EnvironmentRegistrationKeyRevokeParams,
    ) -> Result<api::EnvironmentRegistrationKeyRevokeResponse, AgentApiError> {
        self.request(api::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_REVOKE, params)
            .await
    }

    /// Native Platform routes are described by its generated catalog; authentication
    /// and response handling share the resource HTTP transport.
    pub(crate) async fn platform_request<P: Serialize, R: DeserializeOwned>(
        &self,
        operation: &str,
        params: P,
    ) -> Result<R, AgentApiError> {
        let result: anyhow::Result<R> = async {
            if !self.connection.is_platform() {
                anyhow::bail!("this command requires a Platform deployment");
            }
            let parts =
                crate::platform_api::request_parts(operation, serde_json::to_value(params)?)?;
            let response = self
                .send_parts(parts, self.connection.auth_headers()?)
                .await?;
            response
                .json::<R>()
                .await
                .map_err(|_| anyhow::anyhow!("invalid Platform API response"))
        }
        .await;
        result.map_err(|error| match error.downcast::<AgentApiError>() {
            Ok(error) => error,
            Err(error) => AgentApiError::internal(error.to_string()),
        })
    }

    pub(crate) async fn call_platform(
        &self,
        operation: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AgentApiError> {
        self.platform_request(operation, params).await
    }

    pub(crate) async fn request<P, R>(&self, method: &str, params: P) -> Result<R, AgentApiError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        self.request_connection(method, params)
            .await
            .map_err(|error| match error.downcast::<AgentApiError>() {
                Ok(error) => error,
                Err(error) => AgentApiError::internal(error.to_string()),
            })
    }

    /// Retains typed transport failures for bounded startup retries. Ordinary API
    /// calls still expose the public error contract and are never retried here.
    pub(crate) async fn request_connection<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> anyhow::Result<R> {
        let response = self.send(method, serde_json::to_value(params)?).await?;
        response
            .json::<R>()
            .await
            .map_err(|_| AgentApiError::internal("invalid API response").into())
    }

    async fn send(
        &self,
        operation: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<reqwest::Response> {
        let parts = api::http_request(operation, self.connection.universe.as_deref(), params)?;
        self.send_parts(parts, self.connection.headers(operation)?)
            .await
    }

    async fn send_parts(
        &self,
        parts: api::HttpRequestParts,
        headers: reqwest::header::HeaderMap,
    ) -> anyhow::Result<reqwest::Response> {
        let endpoint = crate::connection::endpoint_with_policy(
            &self.connection.endpoint,
            self.connection.allow_insecure_http,
        )?;
        let base = endpoint.trim_end_matches('/');
        let suffix = parts
            .path
            .strip_prefix("/api/v1")
            .ok_or_else(|| AgentApiError::internal("invalid catalog path"))?;
        let mut request = self
            .client
            .request(
                reqwest::Method::from_bytes(parts.method.as_str().as_bytes())?,
                format!("{base}{suffix}"),
            )
            .headers(headers);
        if let Some(body) = parts.body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|error| {
            if error.is_connect() || error.is_timeout() {
                anyhow::Error::new(StartupUnavailable)
            } else {
                anyhow::Error::new(AgentApiError::internal(format!(
                    "deployment unavailable: {error}"
                )))
            }
        })?;
        if !response.status().is_redirection() {
            self.connection
                .accept_session_rotation(response.headers())?;
        }
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let body = response.bytes().await?;
        if let Ok(problem) = serde_json::from_slice::<api::HttpProblem>(&body) {
            let mut error =
                AgentApiError::new(problem.kind, self.connection.redact_error(problem.detail));
            error.model_default_slot = problem.model_default_slot;
            error.submission_id = problem.submission_id;
            error.deleted_universe = problem.deleted_universe;
            return Err(error.into());
        }
        if matches!(status.as_u16(), 502..=504) {
            return Err(StartupUnavailable.into());
        }
        let kind = match status.as_u16() {
            401 => AgentApiErrorKind::Unauthenticated,
            403 => AgentApiErrorKind::Forbidden,
            _ => AgentApiErrorKind::Internal,
        };
        Err(AgentApiError::new(kind, format!("API request failed: HTTP {status}")).into())
    }
}

pub(crate) fn api_error(error: api::AgentApiError) -> anyhow::Error {
    anyhow::anyhow!("{error}")
}

#[derive(Debug)]
pub(crate) struct StartupUnavailable;
impl std::fmt::Display for StartupUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Deployment connection is temporarily unavailable")
    }
}
impl std::error::Error for StartupUnavailable {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn fixture(
        status: u16,
        content_type: &str,
        body: Vec<u8>,
    ) -> (HttpAgentApi, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api/v1", listener.local_addr().unwrap());
        let content_type = content_type.to_owned();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                assert!(stream.read_line(&mut line).await.unwrap() > 0);
                headers.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 {status} Response\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .unwrap();
            stream.get_mut().write_all(&body).await.unwrap();
            headers
        });
        (
            HttpAgentApi::with_connection(crate::connection::ResolvedConnection {
                name: None,
                endpoint,
                secret: Some("fixture-token".into()),
                universe: Some("team space".into()),
                caller: None,
                ..Default::default()
            }),
            task,
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resources_use_encoded_paths_and_direct_results() {
        let (client, task) = fixture(200, "application/json", br#"{"profiles":[]}"#.to_vec()).await;
        let response: serde_json::Value = client.request("profiles/list", json!({})).await.unwrap();
        assert_eq!(response, json!({"profiles":[]}));
        let headers = task.await.unwrap();
        assert!(headers.starts_with("GET /api/v1/universes/team%20space/profiles HTTP/1.1"));
        assert!(headers.contains("authorization: Bearer fixture-token"));
        assert!(!headers.contains("x-lightspeed-universe"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pending_problem_preserves_submission_and_is_not_startup_unavailability() {
        let body = json!({"type":"about:blank","title":"Gateway Timeout","status":504,"detail":"pending","kind":"operationPending","submissionId":"submission-1"});
        let (client, task) = fixture(
            504,
            "application/problem+json",
            serde_json::to_vec(&body).unwrap(),
        )
        .await;
        let error = client
            .request_connection::<_, serde_json::Value>("profiles/list", json!({}))
            .await
            .unwrap_err();
        let error = error.downcast::<AgentApiError>().unwrap();
        assert_eq!(error.kind, AgentApiErrorKind::OperationPending);
        assert_eq!(error.submission_id.as_deref(), Some("submission-1"));
        task.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blob_download_returns_unmodified_binary_bytes() {
        let bytes = vec![0, 255, 128, 1, 10];
        let (client, task) = fixture(200, "application/octet-stream", bytes.clone()).await;
        assert_eq!(
            client
                .read_blob(BlobReadParams {
                    blob_ref: "sha256:test".into()
                })
                .await
                .unwrap(),
            bytes
        );
        assert!(task.await.unwrap().contains("/blobs/sha256%3Atest/content"));
    }
}
