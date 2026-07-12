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
    /// Concise operation label suitable for API indexes and generated JSDoc.
    pub summary: &'static str,
    /// Short operational guidance: lifecycle, concurrency, security, and
    /// prerequisite semantics that are not obvious from the parameter schema.
    pub description: &'static str,
    pub params_type: &'static str,
    pub result_type: &'static str,
    pub register_schemas: fn(&mut schemars::SchemaGenerator) -> MethodSchemas,
}

pub struct MethodSchemas {
    pub params: schemars::Schema,
    pub result: schemars::Schema,
}

macro_rules! api_methods {
    ($($method_const:ident => $service_fn:ident($params:ty) -> $response:ty =>
        [$summary:expr, $description:expr]),+ $(,)?) => {
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
                        summary: $summary,
                        description: $description,
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
    METHOD_INITIALIZE => initialize(InitializeParams) -> InitializeResponse =>
        ["Inspect the Lightspeed protocol", "Returns protocol version, server identity, and supported capabilities without changing universe state."],
    METHOD_SESSION_START => start_session(SessionStartParams) -> SessionStartResponse =>
        ["Create or reopen a session", "Creates a session with optional config/profile setup. Retrying an existing session id returns that session; creation settings apply only when it is first created."],
    METHOD_SESSION_READ => read_session(SessionReadParams) -> SessionReadResponse =>
        ["Read a session", "Returns the current projected session, including sparse config and revisions, lifecycle/run state, active context, and derived tools."],
    METHOD_SESSION_LIST => list_sessions(SessionListParams) -> SessionListResponse =>
        ["List sessions", "Returns a cursor-paginated summary list ordered by most recent update. Pages may shift while sessions are changing."],
    METHOD_SESSION_CONFIG_PUT => put_session_config(SessionConfigPutParams) -> SessionConfigPutResponse =>
        ["Replace session configuration", "Replaces the complete sparse config while the session is idle. Use the current config revision for safe read-modify-write; omitted features are revoked and an identical document is a no-op."],
    METHOD_SESSION_RENAME => rename_session(SessionRenameParams) -> SessionRenameResponse =>
        ["Rename a session", "Sets the display name, or clears it when displayName is omitted."],
    METHOD_SESSION_CLOSE => close_session(SessionCloseParams) -> SessionCloseResponse =>
        ["Close a session", "Closes an idle session and detaches its environment bindings. Force mode cancels active work, drops queued runs, and can recover a session whose workflow is unavailable."],
    METHOD_SESSION_DELETE => delete_session(SessionDeleteParams) -> SessionDeleteResponse =>
        ["Delete a closed session", "Permanently removes session storage after the session has been closed; close active/open sessions first."],
    METHOD_SESSION_EVENTS_READ => read_session_events(SessionEventsReadParams) -> SessionEventsReadResponse =>
        ["Read the session event stream", "Reads events after a cursor and optionally long-polls when caught up. Continue from nextCursor/headCursor and inspect complete/gap rather than assuming an uninterrupted page."],
    METHOD_SESSION_CONTEXT_APPEND => append_context(ContextAppendParams) -> ContextAppendResponse =>
        ["Append keyed session context", "Admits a batch of context entries with per-entry results. Stable keys make same-content retries no-ops; media preprocessing can fail one entry without discarding successful entries."],
    METHOD_SESSION_CONTEXT_REMOVE => remove_context(ContextRemoveParams) -> ContextRemoveResponse =>
        ["Remove keyed session context", "Removes active entries by stable key with per-key results. Missing keys are idempotent no-ops; runtime-reserved run keys cannot be removed."],
    METHOD_SESSION_CONTEXT_COMPACT => compact_context(ContextCompactParams) -> ContextCompactResponse =>
        ["Compact session context", "Runs the configured compaction policy on an open idle session and waits for the resulting context revision."],
    METHOD_SESSION_RUNS_START => start_run(RunStartParams) -> RunStartResponse =>
        ["Start an agent run", "Accepts input or existing context keys and returns once the run is queued/accepted, not when it finishes. Supply submissionId for retry safety, then follow session events or reread the session."],
    METHOD_SESSION_RUNS_CANCEL => cancel_run(RunCancelParams) -> RunCancelResponse =>
        ["Cancel a run", "Requests cancellation of the named queued or active run and returns its current projected state; observe session events for terminal completion."],
    METHOD_SESSION_SKILLS_LIST => list_skills(SkillListParams) -> SkillListResponse =>
        ["List available session skills", "Refreshes the session's configured VFS skill catalog and reports which discovered skills are enabled and active. An absent catalog yields an empty result."],
    METHOD_SESSION_SKILLS_ACTIVE => active_skills(SkillActiveParams) -> SkillActiveResponse =>
        ["List active session skills", "Returns skill instructions currently injected into context, including activation scope and source."],
    METHOD_SESSION_SKILLS_ACTIVATE => activate_skill(SkillActivateParams) -> SkillActivateResponse =>
        ["Activate a session skill", "Loads an enabled skill from the current catalog and injects its instructions into an open idle session. Run-scoped activation is the default."],
    METHOD_SESSION_SKILLS_DEACTIVATE => deactivate_skill(SkillDeactivateParams) -> SkillDeactivateResponse =>
        ["Deactivate a session skill", "Removes an active skill's injected context from an open idle session; the skill must currently be active."],
    METHOD_SESSION_PROFILES_APPLY => apply_profile(ProfileApplyParams) -> ProfileApplyResponse =>
        ["Apply a profile to a session", "Applies a named or inline profile's config, instructions, mounts, and environment setup to an existing session; mutating profile sections require it to be open and idle. Pass current revisions to guard concurrent changes."],
    METHOD_SESSION_MOUNTS_PUT => put_vfs_mount(VfsMountPutParams) -> VfsMountPutResponse =>
        ["Create or replace a session mount", "Binds a snapshot or workspace at a path on an open idle session that grants VFS. Workspace mounts follow that workspace's current head."],
    METHOD_SESSION_MOUNTS_LIST => list_vfs_mounts(VfsMountListParams) -> VfsMountListResponse =>
        ["List session mounts", "Returns the session's snapshot/workspace bindings and access modes."],
    METHOD_SESSION_MOUNTS_DELETE => delete_vfs_mount(VfsMountDeleteParams) -> VfsMountDeleteResponse =>
        ["Delete a session mount", "Removes a binding from an open idle session without deleting its source snapshot or workspace."],
    METHOD_SESSION_ENVIRONMENTS_READ => read_session_environment(SessionEnvironmentReadParams) -> SessionEnvironmentReadResponse =>
        ["Read a session environment binding", "Returns one session-local environment alias joined with current instance/provider availability and activation state."],
    METHOD_SESSION_ENVIRONMENTS_LIST => list_session_environments(SessionEnvironmentListParams) -> SessionEnvironmentListResponse =>
        ["List session environment bindings", "Returns all environment aliases attached to the session and identifies the active tool target, if any."],
    METHOD_SESSION_ENVIRONMENTS_ATTACH => attach_session_environment(SessionEnvironmentAttachParams) -> SessionEnvironmentAttachResponse =>
        ["Attach an environment to a session", "Binds an existing universe environment instance under a session-local alias, optionally activating it. The session must grant environments and allow the provider."],
    METHOD_SESSION_ENVIRONMENTS_ACTIVATE => activate_session_environment(SessionEnvironmentActivateParams) -> SessionEnvironmentActivateResponse =>
        ["Activate a session environment", "Selects an attached, available environment as the process/filesystem tool target while the session is idle."],
    METHOD_SESSION_ENVIRONMENTS_DEACTIVATE => deactivate_session_environment(SessionEnvironmentDeactivateParams) -> SessionEnvironmentDeactivateResponse =>
        ["Deactivate the session environment", "Clears the active environment tool target without detaching any binding or closing the underlying instance."],
    METHOD_SESSION_ENVIRONMENTS_DETACH => detach_session_environment(SessionEnvironmentDetachParams) -> SessionEnvironmentDetachResponse =>
        ["Detach a session environment", "Detaches the session-local binding; detaching the active target requires an idle session and deactivates it first. The universe instance and jobs remain independently owned."],
    METHOD_SESSION_ENVIRONMENTS_CREDENTIALS_BIND => bind_session_environment_credential(SessionEnvironmentCredentialBindParams) -> SessionEnvironmentCredentialBindResponse =>
        ["Bind a credential into an environment", "Maps an environment variable name to an existing grant/provider/direct-secret handle for one session binding. The response exposes only the source handle, never secret material."],
    METHOD_SESSION_ENVIRONMENTS_CREDENTIALS_LIST => list_session_environment_credentials(SessionEnvironmentCredentialListParams) -> SessionEnvironmentCredentialListResponse =>
        ["List environment credential bindings", "Returns variable names and credential source handles for a session environment; resolved secret values are never returned."],
    METHOD_SESSION_ENVIRONMENTS_CREDENTIALS_UNBIND => unbind_session_environment_credential(SessionEnvironmentCredentialUnbindParams) -> SessionEnvironmentCredentialUnbindResponse =>
        ["Unbind an environment credential", "Removes one variable-to-credential mapping without deleting the underlying grant, provider credential, or secret."],
    METHOD_ENVIRONMENTS_CREATE => create_environment(EnvironmentCreateParams) -> EnvironmentCreateResponse =>
        ["Provision an environment instance", "Asks a live provider with create capability to create a universe-owned environment instance. This does not attach the instance to any session."],
    METHOD_ENVIRONMENTS_READ => read_environment(EnvironmentReadParams) -> EnvironmentReadResponse =>
        ["Read an environment instance", "Returns the universe resource with its provider identity, current observed lifecycle, connection, scope, and capabilities."],
    METHOD_ENVIRONMENTS_LIST => list_environments(EnvironmentListParams) -> EnvironmentListResponse =>
        ["List environment instances", "Lists universe-owned instances, optionally filtered by provider or observed target status."],
    METHOD_ENVIRONMENTS_CLOSE => close_environment(EnvironmentCloseParams) -> EnvironmentCloseResponse =>
        ["Close an environment instance", "Tears down the universe resource through its provider. Closing is rejected while session bindings or nonterminal jobs still occupy the instance."],
    METHOD_ENVIRONMENTS_JOBS_CREATE => create_environment_jobs(EnvironmentJobCreateParams) -> EnvironmentJobCreateResponse =>
        ["Create environment jobs", "Starts a dependency-aware job group on one environment instance. requestId is the retry identity; jobs are owned by the instance rather than a session."],
    METHOD_ENVIRONMENTS_JOBS_READ => read_environment_jobs(EnvironmentJobReadParams) -> EnvironmentJobReadResponse =>
        ["Read environment jobs", "Reads selected job handles with bounded output, optional sequence continuation, and optional artifacts; use returned status/sequence data for polling."],
    METHOD_ENVIRONMENTS_JOBS_LIST => list_environment_jobs(EnvironmentJobListParams) -> EnvironmentJobListResponse =>
        ["List environment jobs", "Lists durable job records across the universe, optionally narrowed to an instance or job group."],
    METHOD_ENVIRONMENTS_JOBS_CANCEL => cancel_environment_jobs(EnvironmentJobCancelParams) -> EnvironmentJobCancelResponse =>
        ["Cancel environment jobs", "Requests cancellation for selected jobs, optionally including dependents. Force is provider-specific escalation; inspect each per-job result."],
    METHOD_MODELS_LIST => list_models(ModelListParams) -> ModelListResponse =>
        ["Discover available models", "Queries supported providers directly on every call and returns best-effort selectable routes. One provider failure does not discard successful results from others."],
    METHOD_PROFILES_CREATE => create_profile(ProfileCreateParams) -> ProfileCreateResponse =>
        ["Create an agent profile", "Creates a new universe-scoped reusable profile document; use profiles/put for create-or-replace revision semantics."],
    METHOD_PROFILES_READ => read_profile(ProfileReadParams) -> ProfileReadResponse =>
        ["Read an agent profile", "Returns the complete profile document and current revision."],
    METHOD_PROFILES_LIST => list_profiles(ProfileListParams) -> ProfileListResponse =>
        ["List agent profiles", "Returns lightweight summaries of universe-scoped reusable profiles."],
    METHOD_PROFILES_PUT => put_profile(ProfilePutParams) -> ProfilePutResponse =>
        ["Create or replace an agent profile", "Stores the complete profile document. Use expectedRevision from profiles/read when replacing to prevent lost updates; absence writes unconditionally."],
    METHOD_PROFILES_DELETE => delete_profile(ProfileDeleteParams) -> ProfileDeleteResponse =>
        ["Delete an agent profile", "Deletes the catalog document; sessions previously created or configured from it retain their materialized state."],
    METHOD_BLOBS_PUT => put_blobs(BlobPutParams) -> BlobPutResponse =>
        ["Store content-addressed blobs", "Decodes and stores a batch of base64 payloads, returning immutable content references in request order. Re-uploading identical bytes is naturally deduplicated."],
    METHOD_BLOBS_READ => read_blob(BlobReadParams) -> BlobReadResponse =>
        ["Read a content-addressed blob", "Returns the complete immutable blob as base64; large values count against gateway and MCP response limits."],
    METHOD_BLOBS_HAS => has_blobs(BlobHasParams) -> BlobHasResponse =>
        ["Check blob availability", "Checks a batch of content references without returning blob bodies, preserving request order."],
    METHOD_VFS_SNAPSHOTS_COMMIT => commit_vfs_snapshot(VfsSnapshotCommitParams) -> VfsSnapshotCommitResponse =>
        ["Commit a VFS snapshot", "Validates and stores an immutable filesystem manifest. Upload referenced file blobs first; the returned snapshot ref is content-addressed."],
    METHOD_VFS_SNAPSHOTS_READ => read_vfs_snapshot(VfsSnapshotReadParams) -> VfsSnapshotReadResponse =>
        ["Read a VFS snapshot", "Returns an immutable snapshot manifest and aggregate file/byte counts; file bodies remain separate blobs."],
    METHOD_VFS_WORKSPACES_CREATE => create_vfs_workspace(VfsWorkspaceCreateParams) -> VfsWorkspaceCreateResponse =>
        ["Create a mutable VFS workspace", "Creates a universe workspace at an optional seed snapshot; absence starts from a server-created empty snapshot."],
    METHOD_VFS_WORKSPACES_READ => read_vfs_workspace(VfsWorkspaceReadParams) -> VfsWorkspaceReadResponse =>
        ["Read a VFS workspace", "Returns workspace metadata, current head snapshot, and revision for safe updates."],
    METHOD_VFS_WORKSPACES_LIST => list_vfs_workspaces(VfsWorkspaceListParams) -> VfsWorkspaceListResponse =>
        ["List VFS workspaces", "Lists mutable universe workspaces with head snapshots, sizes, and revisions."],
    METHOD_VFS_WORKSPACES_UPDATE => update_vfs_workspace(VfsWorkspaceUpdateParams) -> VfsWorkspaceUpdateResponse =>
        ["Update a VFS workspace", "Moves the workspace head to an existing snapshot and updates its display name. Pass expectedRevision from a read to prevent lost updates."],
    METHOD_VFS_WORKSPACES_DELETE => delete_vfs_workspace(VfsWorkspaceDeleteParams) -> VfsWorkspaceDeleteResponse =>
        ["Delete a VFS workspace", "Deletes the mutable workspace record; immutable snapshots and blobs remain content-addressed resources."],
    METHOD_MCP_SERVERS_PUT => put_mcp_server(McpServerPutParams) -> McpServerPutResponse =>
        ["Create or replace an MCP server record", "Stores the complete universe catalog document. Use expectedRevision when replacing; authenticated policies reference grants but never embed credentials."],
    METHOD_MCP_SERVERS_READ => read_mcp_server(McpServerReadParams) -> McpServerReadResponse =>
        ["Read an MCP server record", "Returns one catalog document with defaults, auth policy, status, and revision; no credential value is exposed."],
    METHOD_MCP_SERVERS_LIST => list_mcp_servers(McpServerListParams) -> McpServerListResponse =>
        ["List MCP server records", "Lists universe catalog entries, optionally filtered by lifecycle/configuration status."],
    METHOD_MCP_SERVERS_DELETE => delete_mcp_server(McpServerDeleteParams) -> McpServerDeleteResponse =>
        ["Delete an MCP server record", "Deletes the catalog document. Existing session configs that reference it are not silently rewritten and may need explicit reconfiguration."],
    METHOD_ENVIRONMENTS_PROVIDERS_REGISTER => register_environment_provider(EnvironmentProviderRegisterParams) -> EnvironmentProviderRegisterResponse =>
        ["Register environment provider presence", "Publishes a controller endpoint, capabilities, implementation identity, and liveness lease. Intended for trusted provider/bridge infrastructure, not ordinary configuration clients."],
    METHOD_ENVIRONMENTS_PROVIDERS_HEARTBEAT => heartbeat_environment_provider(EnvironmentProviderHeartbeatParams) -> EnvironmentProviderHeartbeatResponse =>
        ["Refresh environment provider presence", "Renews a provider lease and records its complete observed target descriptors. Omitted provided targets may become unknown; intended for provider infrastructure."],
    METHOD_ENVIRONMENTS_PROVIDERS_UNREGISTER => unregister_environment_provider(EnvironmentProviderUnregisterParams) -> EnvironmentProviderUnregisterResponse =>
        ["Unregister environment provider presence", "Marks provider presence offline without deleting its durable environment instance records."],
    METHOD_ENVIRONMENTS_PROVIDERS_LIST => list_environment_providers(EnvironmentProviderListParams) -> EnvironmentProviderListResponse =>
        ["List environment providers", "Lists current provider presence with lease-derived online/stale/offline status, optionally filtered by status or kind."],
    METHOD_AUTH_GRANTS_IMPORT => import_auth_grant(AuthGrantImportParams) -> AuthGrantImportResponse =>
        ["Import a static bearer grant", "Accepts a plaintext token, encrypts it immediately, and returns only grant metadata/token-presence flags. The token can never be read back through the API."],
    METHOD_AUTH_GRANTS_READ => read_auth_grant(AuthGrantReadParams) -> AuthGrantReadResponse =>
        ["Read authentication grant metadata", "Returns principal, provider binding, scopes, audience, expiry, status, and token-presence flags; access and refresh token values are never returned."],
    METHOD_AUTH_GRANTS_LIST => list_auth_grants(AuthGrantListParams) -> AuthGrantListResponse =>
        ["List authentication grants", "Lists non-secret grant metadata for the universe, optionally filtered by status."],
    METHOD_AUTH_GRANTS_REVOKE => revoke_auth_grant(AuthGrantRevokeParams) -> AuthGrantRevokeResponse =>
        ["Revoke an authentication grant", "Marks the grant unusable by token consumers while retaining non-secret audit metadata."],
    METHOD_AUTH_CLIENTS_CREATE => create_auth_client(AuthClientCreateParams) -> AuthClientCreateResponse =>
        ["Register an OAuth client", "Stores provider endpoints and client identity; an optional plaintext client secret is encrypted and represented thereafter only by hasClientSecret."],
    METHOD_AUTH_CLIENTS_READ => read_auth_client(AuthClientReadParams) -> AuthClientReadResponse =>
        ["Read OAuth client metadata", "Returns endpoints, public client identity, defaults, and secret-presence state; the client secret is never returned."],
    METHOD_AUTH_CLIENTS_LIST => list_auth_clients(AuthClientListParams) -> AuthClientListResponse =>
        ["List OAuth clients", "Lists non-secret OAuth client registrations in the universe."],
    METHOD_AUTH_CLIENTS_DELETE => delete_auth_client(AuthClientDeleteParams) -> AuthClientDeleteResponse =>
        ["Delete an OAuth client", "Deletes the client registration and its stored client secret; grants already created from it remain separate records."],
    METHOD_AUTH_FLOWS_START => start_auth_flow(AuthFlowStartParams) -> AuthFlowStartResponse =>
        ["Start an OAuth authorization flow", "Creates a short-lived PKCE flow and returns a browser authorization URL containing one-time state. Treat the URL as sensitive and poll auth/flows/read for completion."],
    METHOD_AUTH_FLOWS_READ => read_auth_flow_status(AuthFlowStatusParams) -> AuthFlowStatusResponse =>
        ["Read OAuth flow status", "Polls a flow's pending/completed/failed/expired state and returns the resulting grant id when authorization succeeds; no token value is exposed."],
    METHOD_AUTH_PROVIDERS_CREATE => create_auth_provider(AuthProviderCreateParams) -> AuthProviderCreateResponse =>
        ["Register an authentication provider", "Creates a model or GitHub credential source. Plaintext API keys/private keys are encrypted on receipt and later represented only by configuration plus hasCredential."],
    METHOD_AUTH_PROVIDERS_READ => read_auth_provider(AuthProviderReadParams) -> AuthProviderReadResponse =>
        ["Read authentication provider metadata", "Returns provider kind, non-secret configuration, credential-presence state, and status; stored credentials are never returned."],
    METHOD_AUTH_PROVIDERS_LIST => list_auth_providers(AuthProviderListParams) -> AuthProviderListResponse =>
        ["List authentication providers", "Lists non-secret model/GitHub provider registrations for the universe."],
    METHOD_AUTH_PROVIDERS_DELETE => delete_auth_provider(AuthProviderDeleteParams) -> AuthProviderDeleteResponse =>
        ["Delete an authentication provider", "Deletes the provider registration and its directly stored credential; separately stored grants remain independent records."],
    METHOD_AUTH_GITHUB_INSTALLATIONS_LIST => list_github_installations(AuthGitHubInstallationListParams) -> AuthGitHubInstallationListResponse =>
        ["List GitHub App installations", "Uses the registered GitHub App provider credential to query accessible installations and returns account/permission metadata without tokens."],
    METHOD_AUTH_GITHUB_INSTALLATIONS_GRANT => grant_github_installation(AuthGitHubInstallationGrantParams) -> AuthGitHubInstallationGrantResponse =>
        ["Grant access to a GitHub App installation", "Creates or refreshes a universe auth grant for one accessible installation. The installation token is brokered internally and never returned."],
    METHOD_OUTBOX_READ => read_outbox(OutboxReadParams) -> OutboxReadResponse =>
        ["Read pending outbound messages", "Cursor-reads or long-polls the universe delivery outbox. Advance with nextAfter, but only outbox/ack marks individual entries delivered or failed."],
    METHOD_OUTBOX_ACK => ack_outbox(OutboxAckParams) -> OutboxAckResponse =>
        ["Acknowledge outbound delivery", "Records delivered or failed delivery for one outbox entry and updates attempt/status state. Intended for messaging delivery workers."],
}

/// JSON-RPC notification methods the server can emit, with payloads described
/// by the [`AgentNotification`] schema.
pub const NOTIFICATION_METHODS: &[&str] = &[
    NOTIFY_SESSION_STARTED,
    NOTIFY_SESSION_STATUS_CHANGED,
    NOTIFY_SESSION_EVENT,
    NOTIFY_SESSION_RUNS_STARTED,
    NOTIFY_SESSION_RUNS_COMPLETED,
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
