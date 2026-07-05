use super::*;

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

/// Authorization scope of a JSON-RPC method: universe-scoped methods act
/// inside the request's resolved universe; operator-scoped methods address
/// the deployment itself and never resolve one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodScope {
    Universe,
    Operator,
}

impl MethodScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Universe => "universe",
            Self::Operator => "operator",
        }
    }
}

/// Wire contract of one JSON-RPC method: its name, scope, the Rust types of
/// its params and result, and a hook registering both schemas with a
/// [`schemars::SchemaGenerator`]. Produced by the same macro invocation that
/// generates the method's dispatcher, so the manifest cannot drift from it.
pub struct MethodSpec {
    pub method: &'static str,
    pub scope: MethodScope,
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
                        scope: MethodScope::Universe,
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
    METHOD_PROFILES_CREATE => create_profile(ProfileCreateParams) -> ProfileCreateResponse,
    METHOD_PROFILES_READ => read_profile(ProfileReadParams) -> ProfileReadResponse,
    METHOD_PROFILES_LIST => list_profiles(ProfileListParams) -> ProfileListResponse,
    METHOD_PROFILES_PUT => put_profile(ProfilePutParams) -> ProfilePutResponse,
    METHOD_PROFILES_UPDATE => update_profile(ProfileUpdateParams) -> ProfileUpdateResponse,
    METHOD_PROFILES_DELETE => delete_profile(ProfileDeleteParams) -> ProfileDeleteResponse,
    METHOD_PROFILES_APPLY => apply_profile(ProfileApplyParams) -> ProfileApplyResponse,
    METHOD_SESSION_UPDATE => update_session(SessionUpdateParams) -> SessionUpdateResponse,
    METHOD_SESSION_TOOLS_UPDATE => update_session_tools(SessionToolsUpdateParams) -> SessionToolsUpdateResponse,
    METHOD_SESSION_READ => read_session(SessionReadParams) -> SessionReadResponse,
    METHOD_SESSION_LIST => list_sessions(SessionListParams) -> SessionListResponse,
    METHOD_SESSION_RENAME => rename_session(SessionRenameParams) -> SessionRenameResponse,
    METHOD_SESSION_EVENTS_READ => read_session_events(SessionEventsReadParams) -> SessionEventsReadResponse,
    METHOD_SESSION_CLOSE => close_session(SessionCloseParams) -> SessionCloseResponse,
    METHOD_CONTEXT_COMPACT => compact_context(ContextCompactParams) -> ContextCompactResponse,
    METHOD_CONTEXT_APPEND => append_context(ContextAppendParams) -> ContextAppendResponse,
    METHOD_CONTEXT_REMOVE => remove_context(ContextRemoveParams) -> ContextRemoveResponse,
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
    METHOD_SESSION_ENVIRONMENT_CREDENTIALS_BIND => bind_session_environment_credential(SessionEnvironmentCredentialBindParams) -> SessionEnvironmentCredentialBindResponse,
    METHOD_SESSION_ENVIRONMENT_CREDENTIALS_LIST => list_session_environment_credentials(SessionEnvironmentCredentialListParams) -> SessionEnvironmentCredentialListResponse,
    METHOD_SESSION_ENVIRONMENT_CREDENTIALS_UNBIND => unbind_session_environment_credential(SessionEnvironmentCredentialUnbindParams) -> SessionEnvironmentCredentialUnbindResponse,
    METHOD_SESSION_JOBS_CREATE => create_session_jobs(SessionJobCreateParams) -> SessionJobCreateResponse,
    METHOD_SESSION_JOBS_LIST => list_session_jobs(SessionJobListParams) -> SessionJobListResponse,
    METHOD_SESSION_JOBS_READ => read_session_jobs(SessionJobReadParams) -> SessionJobReadResponse,
    METHOD_SESSION_JOBS_CANCEL => cancel_session_jobs(SessionJobCancelParams) -> SessionJobCancelResponse,
    METHOD_ENVIRONMENT_PROVIDERS_REGISTER => register_environment_provider(EnvironmentProviderRegisterParams) -> EnvironmentProviderRegisterResponse,
    METHOD_ENVIRONMENT_PROVIDERS_HEARTBEAT => heartbeat_environment_provider(EnvironmentProviderHeartbeatParams) -> EnvironmentProviderHeartbeatResponse,
    METHOD_ENVIRONMENT_PROVIDERS_UNREGISTER => unregister_environment_provider(EnvironmentProviderUnregisterParams) -> EnvironmentProviderUnregisterResponse,
    METHOD_ENVIRONMENT_PROVIDERS_LIST => list_environment_providers(EnvironmentProviderListParams) -> EnvironmentProviderListResponse,
    METHOD_ENVIRONMENT_PROVIDER_TARGETS_LIST => list_environment_provider_targets(EnvironmentProviderTargetListParams) -> EnvironmentProviderTargetListResponse,
    METHOD_BLOB_PUT => put_blob(BlobPutParams) -> BlobPutResponse,
    METHOD_BLOB_PUT_MANY => put_blobs(BlobPutManyParams) -> BlobPutManyResponse,
    METHOD_BLOB_GET => get_blob(BlobGetParams) -> BlobGetResponse,
    METHOD_BLOB_HAS_MANY => has_blobs(BlobHasManyParams) -> BlobHasManyResponse,
    METHOD_VFS_SNAPSHOT_COMMIT => commit_vfs_snapshot(VfsSnapshotCommitParams) -> VfsSnapshotCommitResponse,
    METHOD_VFS_SNAPSHOT_READ => read_vfs_snapshot(VfsSnapshotReadParams) -> VfsSnapshotReadResponse,
    METHOD_VFS_WORKSPACE_CREATE => create_vfs_workspace(VfsWorkspaceCreateParams) -> VfsWorkspaceCreateResponse,
    METHOD_VFS_WORKSPACE_READ => read_vfs_workspace(VfsWorkspaceReadParams) -> VfsWorkspaceReadResponse,
    METHOD_VFS_WORKSPACE_LIST => list_vfs_workspaces(VfsWorkspaceListParams) -> VfsWorkspaceListResponse,
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

pub(crate) fn json_rpc_params<T>(params: Option<Value>) -> Result<T, JsonRpcError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(params.unwrap_or_else(|| Value::Object(Default::default())))
        .map_err(|error| JsonRpcError::invalid_params(error.to_string()))
}

pub(crate) fn json_rpc_outcome<T>(
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
