use super::*;

#[async_trait]
pub trait AgentApiService: Send + Sync {
    async fn start_transcription(
        &self,
        _params: TranscriptionStartParams,
    ) -> Result<TranscriptionResponse, AgentApiError> {
        Err(AgentApiError::internal("transcription is unavailable"))
    }
    async fn read_transcription(
        &self,
        _params: TranscriptionReadParams,
    ) -> Result<TranscriptionResponse, AgentApiError> {
        Err(AgentApiError::internal("transcription is unavailable"))
    }
    async fn cancel_transcription(
        &self,
        _params: TranscriptionCancelParams,
    ) -> Result<TranscriptionResponse, AgentApiError> {
        Err(AgentApiError::internal("transcription is unavailable"))
    }

    async fn read_model_defaults(
        &self,
        _params: ModelDefaultsReadParams,
    ) -> Result<ModelDefaultsResponse, AgentApiError> {
        Err(AgentApiError::internal("model defaults are unavailable"))
    }

    async fn put_model_defaults(
        &self,
        _params: ModelDefaultsPutParams,
    ) -> Result<ModelDefaultsResponse, AgentApiError> {
        Err(AgentApiError::internal("model defaults are unavailable"))
    }

    async fn list_models(
        &self,
        params: ModelListParams,
    ) -> Result<ModelListResponse, AgentApiError>;

    async fn start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<SessionStartResponse, AgentApiError>;

    async fn create_profile(
        &self,
        params: ProfileCreateParams,
    ) -> Result<ProfileCreateResponse, AgentApiError>;

    async fn read_profile(
        &self,
        params: ProfileReadParams,
    ) -> Result<ProfileReadResponse, AgentApiError>;

    async fn list_profiles(
        &self,
        params: ProfileListParams,
    ) -> Result<ProfileListResponse, AgentApiError>;

    async fn put_profile(
        &self,
        params: ProfilePutParams,
    ) -> Result<ProfilePutResponse, AgentApiError>;

    async fn delete_profile(
        &self,
        params: ProfileDeleteParams,
    ) -> Result<ProfileDeleteResponse, AgentApiError>;

    async fn apply_profile(
        &self,
        params: ProfileApplyParams,
    ) -> Result<ProfileApplyResponse, AgentApiError>;

    async fn put_session_config(
        &self,
        params: SessionConfigPutParams,
    ) -> Result<SessionConfigPutResponse, AgentApiError>;

    async fn read_session(
        &self,
        params: SessionReadParams,
    ) -> Result<SessionReadResponse, AgentApiError>;

    async fn list_sessions(
        &self,
        params: SessionListParams,
    ) -> Result<SessionListResponse, AgentApiError>;

    async fn rename_session(
        &self,
        params: SessionRenameParams,
    ) -> Result<SessionRenameResponse, AgentApiError>;

    async fn put_session_metadata(
        &self,
        params: SessionMetadataPutParams,
    ) -> Result<SessionMetadataPutResponse, AgentApiError>;

    async fn put_session_retention(
        &self,
        _params: SessionRetentionPutParams,
    ) -> Result<SessionRetentionPutResponse, AgentApiError> {
        Err(AgentApiError::internal("session retention is unavailable"))
    }

    async fn read_session_events(
        &self,
        params: SessionEventsReadParams,
    ) -> Result<SessionEventsReadResponse, AgentApiError>;

    async fn restart_session(
        &self,
        _params: SessionRestartParams,
    ) -> Result<SessionRestartResponse, AgentApiError> {
        Err(AgentApiError::internal("session restart is unavailable"))
    }

    async fn close_session(
        &self,
        params: SessionCloseParams,
    ) -> Result<SessionCloseResponse, AgentApiError>;

    async fn share_session(
        &self,
        _params: SessionShareParams,
    ) -> Result<SessionShareResponse, AgentApiError> {
        Err(AgentApiError::internal("session sharing is unavailable"))
    }

    async fn delete_session(
        &self,
        params: SessionDeleteParams,
    ) -> Result<SessionDeleteResponse, AgentApiError>;

    async fn compact_context(
        &self,
        params: ContextCompactParams,
    ) -> Result<ContextCompactResponse, AgentApiError>;

    async fn append_context(
        &self,
        params: ContextAppendParams,
    ) -> Result<ContextAppendResponse, AgentApiError>;

    async fn remove_context(
        &self,
        params: ContextRemoveParams,
    ) -> Result<ContextRemoveResponse, AgentApiError>;

    async fn replace_context(
        &self,
        params: ContextReplaceParams,
    ) -> Result<ContextReplaceResponse, AgentApiError>;

    async fn start_run(&self, params: RunStartParams) -> Result<RunStartResponse, AgentApiError>;

    async fn list_runs(&self, params: RunListParams) -> Result<RunListResponse, AgentApiError>;

    async fn read_run(&self, params: RunReadParams) -> Result<RunReadResponse, AgentApiError>;

    async fn cancel_run(&self, params: RunCancelParams)
    -> Result<RunCancelResponse, AgentApiError>;

    async fn decide_run_approvals(
        &self,
        params: RunApprovalsDecideParams,
    ) -> Result<RunApprovalsDecideResponse, AgentApiError>;

    async fn steer_run(&self, params: RunSteerParams) -> Result<RunSteerResponse, AgentApiError>;

    async fn list_skills(
        &self,
        params: SkillListParams,
    ) -> Result<SkillListResponse, AgentApiError>;

    async fn activate_session_environment(
        &self,
        params: SessionEnvironmentActivateParams,
    ) -> Result<SessionEnvironmentActivateResponse, AgentApiError>;

    async fn deactivate_session_environment(
        &self,
        params: SessionEnvironmentDeactivateParams,
    ) -> Result<SessionEnvironmentDeactivateResponse, AgentApiError>;

    async fn create_environment(
        &self,
        params: EnvironmentCreateParams,
    ) -> Result<EnvironmentCreateResponse, AgentApiError>;

    async fn read_environment(
        &self,
        params: EnvironmentReadParams,
    ) -> Result<EnvironmentReadResponse, AgentApiError>;

    async fn update_environment(
        &self,
        params: EnvironmentUpdateParams,
    ) -> Result<EnvironmentUpdateResponse, AgentApiError>;

    async fn list_environments(
        &self,
        params: EnvironmentListParams,
    ) -> Result<EnvironmentListResponse, AgentApiError>;

    async fn close_environment(
        &self,
        params: EnvironmentCloseParams,
    ) -> Result<EnvironmentCloseResponse, AgentApiError>;

    async fn create_external_environment(
        &self,
        params: EnvironmentExternalCreateParams,
    ) -> Result<EnvironmentExternalCreateResponse, AgentApiError>;

    async fn put_registered_environment(
        &self,
        params: EnvironmentRegisteredPutParams,
    ) -> Result<EnvironmentRegisteredPutResponse, AgentApiError>;

    async fn release_registered_environment(
        &self,
        params: EnvironmentRegisteredReleaseParams,
    ) -> Result<EnvironmentRegisteredReleaseResponse, AgentApiError>;

    async fn put_environment_ingress(
        &self,
        params: EnvironmentIngressPutParams,
    ) -> Result<EnvironmentIngressPutResponse, AgentApiError>;

    async fn put_environment_power(
        &self,
        params: EnvironmentPowerPutParams,
    ) -> Result<EnvironmentPowerPutResponse, AgentApiError>;

    async fn list_available_environment_providers(
        &self,
        _params: AvailableEnvironmentProviderListParams,
    ) -> Result<AvailableEnvironmentProviderListResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "environment providers are unavailable",
        ))
    }

    async fn read_available_environment_provider(
        &self,
        _params: AvailableEnvironmentProviderReadParams,
    ) -> Result<AvailableEnvironmentProviderReadResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "environment providers are unavailable",
        ))
    }

    async fn list_environment_templates(
        &self,
        _params: EnvironmentTemplateListParams,
    ) -> Result<EnvironmentTemplateListResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "environment templates are unavailable",
        ))
    }

    async fn read_environment_template(
        &self,
        _params: EnvironmentTemplateReadParams,
    ) -> Result<EnvironmentTemplateReadResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "environment templates are unavailable",
        ))
    }

    async fn bind_environment_credential(
        &self,
        params: EnvironmentCredentialBindParams,
    ) -> Result<EnvironmentCredentialBindResponse, AgentApiError>;

    async fn list_environment_credentials(
        &self,
        params: EnvironmentCredentialListParams,
    ) -> Result<EnvironmentCredentialListResponse, AgentApiError>;

    async fn unbind_environment_credential(
        &self,
        params: EnvironmentCredentialUnbindParams,
    ) -> Result<EnvironmentCredentialUnbindResponse, AgentApiError>;

    async fn create_environment_jobs(
        &self,
        params: EnvironmentJobCreateParams,
    ) -> Result<EnvironmentJobCreateResponse, AgentApiError>;

    async fn read_environment_jobs(
        &self,
        params: EnvironmentJobReadParams,
    ) -> Result<EnvironmentJobReadResponse, AgentApiError>;

    async fn cancel_environment_jobs(
        &self,
        params: EnvironmentJobCancelParams,
    ) -> Result<EnvironmentJobCancelResponse, AgentApiError>;

    async fn create_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyCreateParams,
    ) -> Result<EnvironmentRegistrationKeyCreateResponse, AgentApiError>;

    async fn put_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyPutParams,
    ) -> Result<EnvironmentRegistrationKeyPutResponse, AgentApiError>;

    async fn read_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyReadParams,
    ) -> Result<EnvironmentRegistrationKeyReadResponse, AgentApiError>;

    async fn list_environment_registration_keys(
        &self,
        params: EnvironmentRegistrationKeyListParams,
    ) -> Result<EnvironmentRegistrationKeyListResponse, AgentApiError>;

    async fn update_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyUpdateParams,
    ) -> Result<EnvironmentRegistrationKeyUpdateResponse, AgentApiError>;

    async fn rotate_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyRotateParams,
    ) -> Result<EnvironmentRegistrationKeyRotateResponse, AgentApiError>;

    async fn revoke_environment_registration_key(
        &self,
        params: EnvironmentRegistrationKeyRevokeParams,
    ) -> Result<EnvironmentRegistrationKeyRevokeResponse, AgentApiError>;

    async fn put_blobs(&self, params: BlobPutParams) -> Result<BlobPutResponse, AgentApiError>;

    async fn has_blobs(&self, params: BlobHasParams) -> Result<BlobHasResponse, AgentApiError>;

    async fn compose_blob(&self, params: BlobComposeParams)
    -> Result<BlobPutResult, AgentApiError>;

    async fn commit_vfs_snapshot(
        &self,
        params: VfsSnapshotCommitParams,
    ) -> Result<VfsSnapshotCommitResponse, AgentApiError>;

    async fn read_vfs_snapshot(
        &self,
        params: VfsSnapshotReadParams,
    ) -> Result<VfsSnapshotReadResponse, AgentApiError>;

    async fn create_vfs_workspace(
        &self,
        params: VfsWorkspaceCreateParams,
    ) -> Result<VfsWorkspaceCreateResponse, AgentApiError>;

    async fn read_vfs_workspace(
        &self,
        params: VfsWorkspaceReadParams,
    ) -> Result<VfsWorkspaceReadResponse, AgentApiError>;

    async fn list_vfs_workspaces(
        &self,
        params: VfsWorkspaceListParams,
    ) -> Result<VfsWorkspaceListResponse, AgentApiError>;

    async fn update_vfs_workspace(
        &self,
        params: VfsWorkspaceUpdateParams,
    ) -> Result<VfsWorkspaceUpdateResponse, AgentApiError>;

    async fn delete_vfs_workspace(
        &self,
        params: VfsWorkspaceDeleteParams,
    ) -> Result<VfsWorkspaceDeleteResponse, AgentApiError>;

    /// Create-or-replace an MCP server document. `expected_revision` is
    /// checked only when the record already exists.
    async fn put_mcp_server(
        &self,
        params: McpServerPutParams,
    ) -> Result<McpServerPutResponse, AgentApiError>;

    async fn discover_mcp_server_auth(
        &self,
        params: McpServerAuthDiscoverParams,
    ) -> Result<McpServerAuthDiscoverResponse, AgentApiError>;

    async fn discover_mcp_server_tools(
        &self,
        params: McpServerToolsDiscoverParams,
    ) -> Result<McpServerToolsDiscoverResponse, AgentApiError>;

    async fn list_mcp_servers(
        &self,
        params: McpServerListParams,
    ) -> Result<McpServerListResponse, AgentApiError>;

    async fn read_mcp_server(
        &self,
        params: McpServerReadParams,
    ) -> Result<McpServerReadResponse, AgentApiError>;

    async fn delete_mcp_server(
        &self,
        params: McpServerDeleteParams,
    ) -> Result<McpServerDeleteResponse, AgentApiError>;

    async fn import_credential_grant(
        &self,
        params: CredentialGrantImportParams,
    ) -> Result<CredentialGrantImportResponse, AgentApiError>;

    async fn lease_credential_grant(
        &self,
        params: CredentialGrantLeaseParams,
    ) -> Result<CredentialGrantLeaseResponse, AgentApiError>;

    async fn list_credential_grants(
        &self,
        params: CredentialGrantListParams,
    ) -> Result<CredentialGrantListResponse, AgentApiError>;

    async fn read_credential_grant(
        &self,
        params: CredentialGrantReadParams,
    ) -> Result<CredentialGrantReadResponse, AgentApiError>;

    async fn put_credential_grant(
        &self,
        params: CredentialGrantPutParams,
    ) -> Result<CredentialGrantPutResponse, AgentApiError>;

    async fn revoke_credential_grant(
        &self,
        params: CredentialGrantRevokeParams,
    ) -> Result<CredentialGrantRevokeResponse, AgentApiError>;

    async fn create_credential_client(
        &self,
        params: CredentialClientCreateParams,
    ) -> Result<CredentialClientCreateResponse, AgentApiError>;

    async fn list_credential_clients(
        &self,
        params: CredentialClientListParams,
    ) -> Result<CredentialClientListResponse, AgentApiError>;

    async fn read_credential_client(
        &self,
        params: CredentialClientReadParams,
    ) -> Result<CredentialClientReadResponse, AgentApiError>;

    async fn delete_credential_client(
        &self,
        params: CredentialClientDeleteParams,
    ) -> Result<CredentialClientDeleteResponse, AgentApiError>;

    async fn start_credential_flow(
        &self,
        params: CredentialFlowStartParams,
    ) -> Result<CredentialFlowStartResponse, AgentApiError>;

    async fn read_credential_flow_status(
        &self,
        params: CredentialFlowStatusParams,
    ) -> Result<CredentialFlowStatusResponse, AgentApiError>;

    async fn create_credential_provider(
        &self,
        params: CredentialProviderCreateParams,
    ) -> Result<CredentialProviderCreateResponse, AgentApiError>;

    async fn put_credential_provider(
        &self,
        params: CredentialProviderPutParams,
    ) -> Result<CredentialProviderPutResponse, AgentApiError>;

    async fn list_credential_providers(
        &self,
        params: CredentialProviderListParams,
    ) -> Result<CredentialProviderListResponse, AgentApiError>;

    async fn read_credential_provider(
        &self,
        params: CredentialProviderReadParams,
    ) -> Result<CredentialProviderReadResponse, AgentApiError>;

    async fn delete_credential_provider(
        &self,
        params: CredentialProviderDeleteParams,
    ) -> Result<CredentialProviderDeleteResponse, AgentApiError>;

    async fn list_github_installations(
        &self,
        params: CredentialGitHubInstallationListParams,
    ) -> Result<CredentialGitHubInstallationListResponse, AgentApiError>;

    async fn grant_github_installation(
        &self,
        params: CredentialGitHubInstallationGrantParams,
    ) -> Result<CredentialGitHubInstallationGrantResponse, AgentApiError>;
    // ── Bots ────────────────────────────────────────────────────────────────

    async fn create_bot(&self, params: BotCreateParams)
    -> Result<BotCreateResponse, AgentApiError>;

    async fn put_bot(&self, params: BotPutParams) -> Result<BotPutResponse, AgentApiError>;

    async fn read_bot(&self, params: BotReadParams) -> Result<BotReadResponse, AgentApiError>;

    async fn list_bots(&self, params: BotListParams) -> Result<BotListResponse, AgentApiError>;

    async fn close_bot(&self, params: BotCloseParams) -> Result<BotCloseResponse, AgentApiError>;

    async fn delete_bot(&self, params: BotDeleteParams)
    -> Result<BotDeleteResponse, AgentApiError>;

    async fn read_bot_state(
        &self,
        params: BotStateReadParams,
    ) -> Result<BotStateReadResponse, AgentApiError>;

    async fn rotate_bot_session(
        &self,
        params: BotSessionRotateParams,
    ) -> Result<BotSessionRotateResponse, AgentApiError>;

    async fn put_bot_trigger(
        &self,
        params: BotTriggerPutParams,
    ) -> Result<BotTriggerPutResponse, AgentApiError>;

    async fn read_bot_trigger(
        &self,
        params: BotTriggerReadParams,
    ) -> Result<BotTriggerReadResponse, AgentApiError>;

    async fn list_bot_triggers(
        &self,
        params: BotTriggerListParams,
    ) -> Result<BotTriggerListResponse, AgentApiError>;

    async fn read_bot_trigger_secrets(
        &self,
        params: BotTriggerSecretsReadParams,
    ) -> Result<BotTriggerSecretsReadResponse, AgentApiError>;

    async fn delete_bot_trigger(
        &self,
        params: BotTriggerDeleteParams,
    ) -> Result<BotTriggerDeleteResponse, AgentApiError>;

    async fn admit_bot_event(
        &self,
        params: BotEventAdmitParams,
    ) -> Result<BotEventAdmitResponse, AgentApiError>;

    async fn replay_bot_event(
        &self,
        params: BotEventReplayParams,
    ) -> Result<BotEventReplayResponse, AgentApiError>;

    async fn list_bot_events(
        &self,
        params: BotEventListParams,
    ) -> Result<BotEventListResponse, AgentApiError>;

    async fn read_bot_event(
        &self,
        params: BotEventReadParams,
    ) -> Result<BotEventReadResponse, AgentApiError>;

    async fn test_bot_filter(
        &self,
        params: BotFilterTestParams,
    ) -> Result<BotFilterTestResponse, AgentApiError>;
}
