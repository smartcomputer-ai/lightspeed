use super::*;

#[async_trait]
pub trait AgentApiService: Send + Sync {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> Result<AgentApiOutcome<InitializeResponse>, AgentApiError>;

    async fn list_models(
        &self,
        params: ModelListParams,
    ) -> Result<AgentApiOutcome<ModelListResponse>, AgentApiError>;

    async fn start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError>;

    async fn create_profile(
        &self,
        params: ProfileCreateParams,
    ) -> Result<AgentApiOutcome<ProfileCreateResponse>, AgentApiError>;

    async fn read_profile(
        &self,
        params: ProfileReadParams,
    ) -> Result<AgentApiOutcome<ProfileReadResponse>, AgentApiError>;

    async fn list_profiles(
        &self,
        params: ProfileListParams,
    ) -> Result<AgentApiOutcome<ProfileListResponse>, AgentApiError>;

    async fn put_profile(
        &self,
        params: ProfilePutParams,
    ) -> Result<AgentApiOutcome<ProfilePutResponse>, AgentApiError>;

    async fn delete_profile(
        &self,
        params: ProfileDeleteParams,
    ) -> Result<AgentApiOutcome<ProfileDeleteResponse>, AgentApiError>;

    async fn apply_profile(
        &self,
        params: ProfileApplyParams,
    ) -> Result<AgentApiOutcome<ProfileApplyResponse>, AgentApiError>;

    async fn put_session_config(
        &self,
        params: SessionConfigPutParams,
    ) -> Result<AgentApiOutcome<SessionConfigPutResponse>, AgentApiError>;

    async fn read_session(
        &self,
        params: SessionReadParams,
    ) -> Result<AgentApiOutcome<SessionReadResponse>, AgentApiError>;

    async fn list_sessions(
        &self,
        params: SessionListParams,
    ) -> Result<AgentApiOutcome<SessionListResponse>, AgentApiError>;

    async fn rename_session(
        &self,
        params: SessionRenameParams,
    ) -> Result<AgentApiOutcome<SessionRenameResponse>, AgentApiError>;

    async fn read_session_events(
        &self,
        params: SessionEventsReadParams,
    ) -> Result<AgentApiOutcome<SessionEventsReadResponse>, AgentApiError>;

    async fn close_session(
        &self,
        params: SessionCloseParams,
    ) -> Result<AgentApiOutcome<SessionCloseResponse>, AgentApiError>;

    async fn delete_session(
        &self,
        params: SessionDeleteParams,
    ) -> Result<AgentApiOutcome<SessionDeleteResponse>, AgentApiError>;

    async fn compact_context(
        &self,
        params: ContextCompactParams,
    ) -> Result<AgentApiOutcome<ContextCompactResponse>, AgentApiError>;

    async fn append_context(
        &self,
        params: ContextAppendParams,
    ) -> Result<AgentApiOutcome<ContextAppendResponse>, AgentApiError>;

    async fn remove_context(
        &self,
        params: ContextRemoveParams,
    ) -> Result<AgentApiOutcome<ContextRemoveResponse>, AgentApiError>;

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

    async fn detach_session_environment(
        &self,
        params: SessionEnvironmentDetachParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentDetachResponse>, AgentApiError>;

    async fn create_environment(
        &self,
        params: EnvironmentCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentCreateResponse>, AgentApiError>;

    async fn read_environment(
        &self,
        params: EnvironmentReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentReadResponse>, AgentApiError>;

    async fn list_environments(
        &self,
        params: EnvironmentListParams,
    ) -> Result<AgentApiOutcome<EnvironmentListResponse>, AgentApiError>;

    async fn close_environment(
        &self,
        params: EnvironmentCloseParams,
    ) -> Result<AgentApiOutcome<EnvironmentCloseResponse>, AgentApiError>;

    async fn bind_session_environment_credential(
        &self,
        params: SessionEnvironmentCredentialBindParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentCredentialBindResponse>, AgentApiError>;

    async fn list_session_environment_credentials(
        &self,
        params: SessionEnvironmentCredentialListParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentCredentialListResponse>, AgentApiError>;

    async fn unbind_session_environment_credential(
        &self,
        params: SessionEnvironmentCredentialUnbindParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentCredentialUnbindResponse>, AgentApiError>;

    async fn create_environment_jobs(
        &self,
        params: EnvironmentJobCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobCreateResponse>, AgentApiError>;

    async fn read_environment_jobs(
        &self,
        params: EnvironmentJobReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobReadResponse>, AgentApiError>;

    async fn list_environment_jobs(
        &self,
        params: EnvironmentJobListParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobListResponse>, AgentApiError>;

    async fn cancel_environment_jobs(
        &self,
        params: EnvironmentJobCancelParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobCancelResponse>, AgentApiError>;

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

    async fn list_environment_providers(
        &self,
        params: EnvironmentProviderListParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderListResponse>, AgentApiError>;

    async fn put_blobs(
        &self,
        params: BlobPutParams,
    ) -> Result<AgentApiOutcome<BlobPutResponse>, AgentApiError>;

    async fn read_blob(
        &self,
        params: BlobReadParams,
    ) -> Result<AgentApiOutcome<BlobReadResponse>, AgentApiError>;

    async fn has_blobs(
        &self,
        params: BlobHasParams,
    ) -> Result<AgentApiOutcome<BlobHasResponse>, AgentApiError>;

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

    async fn list_vfs_workspaces(
        &self,
        params: VfsWorkspaceListParams,
    ) -> Result<AgentApiOutcome<VfsWorkspaceListResponse>, AgentApiError>;

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

    /// Create-or-replace an MCP server document. `expected_revision` is
    /// checked only when the record already exists.
    async fn put_mcp_server(
        &self,
        params: McpServerPutParams,
    ) -> Result<AgentApiOutcome<McpServerPutResponse>, AgentApiError>;

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
