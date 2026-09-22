use super::*;

#[async_trait]
pub trait AgentApiService: Send + Sync {
    async fn read_access(
        &self,
        _params: AccessReadParams,
    ) -> Result<AgentApiOutcome<AccessReadResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "action permissions are unavailable",
        ))
    }
    async fn read_access_policy(
        &self,
        _params: AccessPolicyReadParams,
    ) -> Result<AgentApiOutcome<AccessPolicyReadResponse>, AgentApiError> {
        Err(AgentApiError::internal("access policies are unavailable"))
    }
    async fn put_access_policy(
        &self,
        _params: AccessPolicyPutParams,
    ) -> Result<AgentApiOutcome<AccessPolicyPutResponse>, AgentApiError> {
        Err(AgentApiError::internal("access policies are unavailable"))
    }

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

    async fn start_managed_session(
        &self,
        params: ManagedSessionStartParams,
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

    async fn put_session_metadata(
        &self,
        params: SessionMetadataPutParams,
    ) -> Result<AgentApiOutcome<SessionMetadataPutResponse>, AgentApiError>;

    async fn put_session_retention(
        &self,
        _params: SessionRetentionPutParams,
    ) -> Result<AgentApiOutcome<SessionRetentionPutResponse>, AgentApiError> {
        Err(AgentApiError::internal("session retention is unavailable"))
    }

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

    async fn start_run(
        &self,
        params: RunStartParams,
    ) -> Result<AgentApiOutcome<RunStartResponse>, AgentApiError>;

    async fn list_runs(
        &self,
        params: RunListParams,
    ) -> Result<AgentApiOutcome<RunListResponse>, AgentApiError>;

    async fn read_run(
        &self,
        params: RunReadParams,
    ) -> Result<AgentApiOutcome<RunReadResponse>, AgentApiError>;

    async fn cancel_run(
        &self,
        params: RunCancelParams,
    ) -> Result<AgentApiOutcome<RunCancelResponse>, AgentApiError>;

    async fn decide_run_approvals(
        &self,
        params: RunApprovalsDecideParams,
    ) -> Result<AgentApiOutcome<RunApprovalsDecideResponse>, AgentApiError>;

    async fn steer_run(
        &self,
        params: RunSteerParams,
    ) -> Result<AgentApiOutcome<RunSteerResponse>, AgentApiError>;

    async fn list_skills(
        &self,
        params: SkillListParams,
    ) -> Result<AgentApiOutcome<SkillListResponse>, AgentApiError>;

    async fn activate_session_environment(
        &self,
        params: SessionEnvironmentActivateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentActivateResponse>, AgentApiError>;

    async fn deactivate_session_environment(
        &self,
        params: SessionEnvironmentDeactivateParams,
    ) -> Result<AgentApiOutcome<SessionEnvironmentDeactivateResponse>, AgentApiError>;

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

    async fn create_external_environment(
        &self,
        params: EnvironmentExternalCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentExternalCreateResponse>, AgentApiError>;

    async fn put_environment_ingress(
        &self,
        params: EnvironmentIngressPutParams,
    ) -> Result<AgentApiOutcome<EnvironmentIngressPutResponse>, AgentApiError>;

    async fn put_environment_power(
        &self,
        params: EnvironmentPowerPutParams,
    ) -> Result<AgentApiOutcome<EnvironmentPowerPutResponse>, AgentApiError>;

    async fn put_environment_idle_policy(
        &self,
        params: EnvironmentIdlePolicyPutParams,
    ) -> Result<AgentApiOutcome<EnvironmentIdlePolicyPutResponse>, AgentApiError>;

    async fn list_environment_provider_bindings(
        &self,
        _params: EnvironmentProviderBindingListParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderBindingListResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "environment provider bindings are unavailable",
        ))
    }

    async fn read_environment_provider_binding(
        &self,
        _params: EnvironmentProviderBindingReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderBindingReadResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "environment provider bindings are unavailable",
        ))
    }

    async fn list_environment_templates(
        &self,
        _params: EnvironmentTemplateListParams,
    ) -> Result<AgentApiOutcome<EnvironmentTemplateListResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "environment templates are unavailable",
        ))
    }

    async fn read_environment_template(
        &self,
        _params: EnvironmentTemplateReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentTemplateReadResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "environment templates are unavailable",
        ))
    }

    async fn bind_environment_credential(
        &self,
        params: EnvironmentCredentialBindParams,
    ) -> Result<AgentApiOutcome<EnvironmentCredentialBindResponse>, AgentApiError>;

    async fn list_environment_credentials(
        &self,
        params: EnvironmentCredentialListParams,
    ) -> Result<AgentApiOutcome<EnvironmentCredentialListResponse>, AgentApiError>;

    async fn unbind_environment_credential(
        &self,
        params: EnvironmentCredentialUnbindParams,
    ) -> Result<AgentApiOutcome<EnvironmentCredentialUnbindResponse>, AgentApiError>;

    async fn create_environment_jobs(
        &self,
        params: EnvironmentJobCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobCreateResponse>, AgentApiError>;

    async fn read_environment_jobs(
        &self,
        params: EnvironmentJobReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobReadResponse>, AgentApiError>;

    async fn cancel_environment_jobs(
        &self,
        params: EnvironmentJobCancelParams,
    ) -> Result<AgentApiOutcome<EnvironmentJobCancelResponse>, AgentApiError>;

    async fn create_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyCreateParams,
    ) -> Result<AgentApiOutcome<EnvironmentRegistrationKeyCreateResponse>, AgentApiError>;

    async fn read_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentRegistrationKeyReadResponse>, AgentApiError>;

    async fn list_environment_registration_keys(
        &self,
        params: EnvironmentRegistrationKeyListParams,
    ) -> Result<AgentApiOutcome<EnvironmentRegistrationKeyListResponse>, AgentApiError>;

    async fn revoke_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyRevokeParams,
    ) -> Result<AgentApiOutcome<EnvironmentRegistrationKeyRevokeResponse>, AgentApiError>;

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

    /// Create-or-replace an MCP server document. `expected_revision` is
    /// checked only when the record already exists.
    async fn put_mcp_server(
        &self,
        params: McpServerPutParams,
    ) -> Result<AgentApiOutcome<McpServerPutResponse>, AgentApiError>;

    async fn discover_mcp_server_auth(
        &self,
        params: McpServerAuthDiscoverParams,
    ) -> Result<AgentApiOutcome<McpServerAuthDiscoverResponse>, AgentApiError>;

    async fn discover_mcp_server_tools(
        &self,
        params: McpServerToolsDiscoverParams,
    ) -> Result<AgentApiOutcome<McpServerToolsDiscoverResponse>, AgentApiError>;

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

    async fn lease_auth_grant(
        &self,
        params: AuthGrantLeaseParams,
    ) -> Result<AgentApiOutcome<AuthGrantLeaseResponse>, AgentApiError>;

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
    // ── Bots ────────────────────────────────────────────────────────────────

    async fn create_bot(
        &self,
        params: BotCreateParams,
    ) -> Result<AgentApiOutcome<BotCreateResponse>, AgentApiError>;

    async fn put_bot(
        &self,
        params: BotPutParams,
    ) -> Result<AgentApiOutcome<BotPutResponse>, AgentApiError>;

    async fn read_bot(
        &self,
        params: BotReadParams,
    ) -> Result<AgentApiOutcome<BotReadResponse>, AgentApiError>;

    async fn list_bots(
        &self,
        params: BotListParams,
    ) -> Result<AgentApiOutcome<BotListResponse>, AgentApiError>;

    async fn close_bot(
        &self,
        params: BotCloseParams,
    ) -> Result<AgentApiOutcome<BotCloseResponse>, AgentApiError>;

    async fn delete_bot(
        &self,
        params: BotDeleteParams,
    ) -> Result<AgentApiOutcome<BotDeleteResponse>, AgentApiError>;

    async fn read_bot_state(
        &self,
        params: BotStateReadParams,
    ) -> Result<AgentApiOutcome<BotStateReadResponse>, AgentApiError>;

    async fn rotate_bot_session(
        &self,
        params: BotSessionRotateParams,
    ) -> Result<AgentApiOutcome<BotSessionRotateResponse>, AgentApiError>;

    async fn put_bot_trigger(
        &self,
        params: BotTriggerPutParams,
    ) -> Result<AgentApiOutcome<BotTriggerPutResponse>, AgentApiError>;

    async fn read_bot_trigger(
        &self,
        params: BotTriggerReadParams,
    ) -> Result<AgentApiOutcome<BotTriggerReadResponse>, AgentApiError>;

    async fn list_bot_triggers(
        &self,
        params: BotTriggerListParams,
    ) -> Result<AgentApiOutcome<BotTriggerListResponse>, AgentApiError>;

    async fn delete_bot_trigger(
        &self,
        params: BotTriggerDeleteParams,
    ) -> Result<AgentApiOutcome<BotTriggerDeleteResponse>, AgentApiError>;

    async fn admit_bot_event(
        &self,
        params: BotEventAdmitParams,
    ) -> Result<AgentApiOutcome<BotEventAdmitResponse>, AgentApiError>;

    async fn replay_bot_event(
        &self,
        params: BotEventReplayParams,
    ) -> Result<AgentApiOutcome<BotEventReplayResponse>, AgentApiError>;

    async fn list_bot_events(
        &self,
        params: BotEventListParams,
    ) -> Result<AgentApiOutcome<BotEventListResponse>, AgentApiError>;

    async fn read_bot_event(
        &self,
        params: BotEventReadParams,
    ) -> Result<AgentApiOutcome<BotEventReadResponse>, AgentApiError>;

    async fn test_bot_filter(
        &self,
        params: BotFilterTestParams,
    ) -> Result<AgentApiOutcome<BotFilterTestResponse>, AgentApiError>;

    // ── Channels ────────────────────────────────────────────────────────────

    async fn create_channel_account(
        &self,
        params: ChannelAccountCreateParams,
    ) -> Result<AgentApiOutcome<ChannelAccountCreateResponse>, AgentApiError>;

    async fn put_channel_account(
        &self,
        params: ChannelAccountPutParams,
    ) -> Result<AgentApiOutcome<ChannelAccountPutResponse>, AgentApiError>;

    async fn read_channel_account(
        &self,
        params: ChannelAccountReadParams,
    ) -> Result<AgentApiOutcome<ChannelAccountReadResponse>, AgentApiError>;

    async fn list_channel_accounts(
        &self,
        params: ChannelAccountListParams,
    ) -> Result<AgentApiOutcome<ChannelAccountListResponse>, AgentApiError>;

    async fn delete_channel_account(
        &self,
        params: ChannelAccountDeleteParams,
    ) -> Result<AgentApiOutcome<ChannelAccountDeleteResponse>, AgentApiError>;

    async fn admit_channel_inbound(
        &self,
        params: ChannelInboundAdmitParams,
    ) -> Result<AgentApiOutcome<ChannelInboundAdmitResponse>, AgentApiError>;

    async fn list_channel_pairings(
        &self,
        params: ChannelPairingListParams,
    ) -> Result<AgentApiOutcome<ChannelPairingListResponse>, AgentApiError>;

    async fn delete_channel_pairing(
        &self,
        params: ChannelPairingDeleteParams,
    ) -> Result<AgentApiOutcome<ChannelPairingDeleteResponse>, AgentApiError>;

    async fn read_channel_conversation(
        &self,
        params: ChannelConversationReadParams,
    ) -> Result<AgentApiOutcome<ChannelConversationReadResponse>, AgentApiError>;
}
