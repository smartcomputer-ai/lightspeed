use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentApiErrorKind {
    InvalidRequest,
    NotFound,
    Conflict,
    /// The request was understood and the caller may make it, but current
    /// state or policy refuses it. Never an authorization decision.
    Rejected,
    /// No valid credential was presented.
    Unauthenticated,
    /// The authenticated caller lacks permission for this operation or target.
    Forbidden,
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
    /// The environment exists but is not reachable yet: it is still
    /// provisioning/booting, or it was powered down and a wake (desired
    /// power `running`) has been initiated by this request. Distinct from
    /// `Rejected` so clients retry with backoff instead of failing —
    /// polling and automation callers lean on this for wake-on-use.
    EnvironmentNotReady,
    /// The requested page still exceeds the public serialized response
    /// budget. Callers can retry with a smaller page limit or continuation.
    ResponseTooLarge,
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

    pub fn unauthenticated() -> Self {
        Self::new(
            AgentApiErrorKind::Unauthenticated,
            "request is not authenticated",
        )
    }

    pub fn forbidden() -> Self {
        Self::new(AgentApiErrorKind::Forbidden, "request is not authorized")
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

    pub fn environment_not_ready(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::EnvironmentNotReady, message)
    }

    pub fn response_too_large(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::ResponseTooLarge, message)
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
            AgentApiErrorKind::Unauthenticated => -32001,
            AgentApiErrorKind::Forbidden => -32003,
            AgentApiErrorKind::NotFound => -32004,
            AgentApiErrorKind::Conflict => -32009,
            AgentApiErrorKind::Rejected
            | AgentApiErrorKind::TranscodeFailure
            | AgentApiErrorKind::TranscriptionFailure => -32010,
            AgentApiErrorKind::SessionBootstrapFailed => -32011,
            AgentApiErrorKind::EnvironmentNotReady => -32012,
            AgentApiErrorKind::ResponseTooLarge => -32013,
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

/// Authorization scope of a JSON-RPC method. Service methods resolve a
/// universe like ordinary universe methods, but are admitted only for trusted
/// service callers at the HTTP edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodScope {
    Universe,
    Service,
    Deployment,
}

pub fn is_service_method(method: &str) -> bool {
    matches!(method_access(method), Some(MethodAccess::Service))
}

impl MethodScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Universe => "universe",
            Self::Service => "service",
            Self::Deployment => "deployment",
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
    pub access: MethodAccess,
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
        [$summary:expr, $description:expr], access: $access:expr),+ $(,)?) => {
        pub(crate) fn universe_method_access(method: &str) -> Option<MethodAccess> {
            match method {
                $($method_const => Some($access),)+
                _ => None,
            }
        }


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
                        scope: ($access).scope(),
                        access: $access,
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
    METHOD_VFS_WORKSPACES_FILES_READ => read_vfs_workspace_file(VfsWorkspaceFileReadParams) -> BlobReadResponse =>
        ["Read a workspace file", "Reads bytes at a path in the current workspace head."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_INITIALIZE => initialize(InitializeParams) -> InitializeResponse =>
        ["Inspect the Lightspeed protocol", "Returns protocol version, server identity, supported capabilities, and the method groups the caller's key may call, without changing universe state. Every key may call it."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_SESSION_START => start_session(SessionStartParams) -> SessionStartResponse =>
        ["Create or reopen a session", "Creates a session, unshared unless access says universe, with optional config/profile setup. Profile metadata and retention supply defaults that explicit values override; the config's default environment attachment becomes active. Retrying an existing id returns that session and keeps its audience."], access: MethodAccess::Universe(UniverseAction::CreateSession),
    METHOD_SESSION_MANAGED_START => start_managed_session(ManagedSessionStartParams) -> SessionStartResponse =>
        ["Create or reopen a managed session", "Creates a session with immutable lifecycle and workflow-tool declarations using explicit bound dispatch. Profile metadata, retention, and default environment attachment selection follow session/start semantics. Retrying an id requires the same managed declaration; an ordinary session cannot be upgraded."], access: MethodAccess::Universe(UniverseAction::CreateSession),
    METHOD_SESSION_READ => read_session(SessionReadParams) -> SessionReadResponse =>
        ["Read a session", "Returns current state plus a bounded newest-first run-summary page. Follow nextRunCursor with session/runs/list when hasOlderRuns is true; use session/events/read for the transcript."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_SESSION_LIST => list_sessions(SessionListParams) -> SessionListResponse =>
        ["List sessions", "Returns a cursor-paginated summary list ordered by most recent update, optionally narrowed by the audience of each session's root: createdBy, visibility, or visibleTo (shared with the universe or created by that actor). Pages may shift while sessions are changing."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_SESSION_CONFIG_PUT => put_session_config(SessionConfigPutParams) -> SessionConfigPutResponse =>
        ["Replace session configuration", "Replaces the complete sparse config while the session is idle. Use the current config revision for safe read-modify-write; omitted features are revoked and an identical document is a no-op."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_SESSION_RENAME => rename_session(SessionRenameParams) -> SessionRenameResponse =>
        ["Rename a session", "Sets the display name, or clears it when displayName is omitted."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_SESSION_METADATA_PUT => put_session_metadata(SessionMetadataPutParams) -> SessionMetadataPutResponse =>
        ["Replace session metadata", "Replaces the complete descriptive key/value map (bounded like session/start); an omitted or empty map clears it. Record-only: the event log and updatedAtMs are untouched."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_SESSION_RETENTION_PUT => put_session_retention(SessionRetentionPutParams) -> SessionRetentionPutResponse =>
        ["Replace session retention", "Sets the positive close-relative automatic-deletion duration on a retention root, or clears it with null. Forks and delegated children inherit the root policy and cannot override it."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_SESSION_CLOSE => close_session(SessionCloseParams) -> SessionCloseResponse =>
        ["Close a session", "Closes an idle session and detaches its environment bindings. Force mode cancels active work, drops queued runs, and can recover a session whose workflow is unavailable."], access: MethodAccess::Universe(UniverseAction::StopSession),
    METHOD_SESSION_DELETE => delete_session(SessionDeleteParams) -> SessionDeleteResponse =>
        ["Delete closed sessions", "Permanently removes a closed retention-tree leaf, or its closed history-fork and delegated-child subtree when cascade is true. Config-only clones are never included."], access: MethodAccess::Universe(UniverseAction::DeleteSession),
    METHOD_SESSION_SHARE => share_session(SessionShareParams) -> SessionShareResponse =>
        ["Share a session with the universe", "Moves an unshared root session to universe visibility, one way; its delegated children follow it. Refused on a bot's session, a delegated child, and a session already shared. Core applies it for any caller of the method; who may share is the caller's gate's decision."], access: MethodAccess::Universe(UniverseAction::ShareSession),
    METHOD_SESSION_EVENTS_READ => read_session_events(SessionEventsReadParams) -> SessionEventsReadResponse =>
        ["Read the session event stream", "Returns chronological events. Forward (default) follows after and supports long-polling. Backward reads the latest window below before (or the head); pass nextCursor as before until complete. Follow live events after the initial backward headCursor. Windows may split runs/tool batches; keep historical reconstruction separate from live controls."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_SESSION_CONTEXT_APPEND => append_context(ContextAppendParams) -> ContextAppendResponse =>
        ["Append keyed session context", "Admits a batch of context entries with per-entry results. Stable keys make same-content retries no-ops; media preprocessing can fail one entry without discarding successful entries."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_SESSION_CONTEXT_REMOVE => remove_context(ContextRemoveParams) -> ContextRemoveResponse =>
        ["Remove keyed session context", "Removes active entries by stable key with per-key results. Missing keys are idempotent no-ops; runtime-reserved run keys cannot be removed."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_SESSION_CONTEXT_COMPACT => compact_context(ContextCompactParams) -> ContextCompactResponse =>
        ["Compact session context", "Runs the configured compaction policy on an open idle session and waits for the resulting context revision."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_SESSION_RUNS_START => start_run(RunStartParams) -> RunStartResponse =>
        ["Start an agent run", "Accepts input or existing context keys and returns once the run is accepted — queued behind an active run, or running — not when it finishes. Supply submissionId for retry safety, then follow session events or reread the session."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_SESSION_RUNS_LIST => list_runs(RunListParams) -> RunListResponse =>
        ["List session runs", "Returns a newest-first keyset page of bounded run summaries projected from current reducer state."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_SESSION_RUNS_READ => read_run(RunReadParams) -> RunReadResponse =>
        ["Read one session run", "Reads and projects one run from its bounded event interval, paged by event sequence."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_SESSION_RUNS_CANCEL => cancel_run(RunCancelParams) -> RunCancelResponse =>
        ["Cancel a run", "Requests cancellation of the named queued or active run and returns its current projected state; observe session events for terminal completion. In-flight model and tool activity is aborted; no grace turn runs."], access: MethodAccess::Universe(UniverseAction::StopSession),
    METHOD_SESSION_RUNS_APPROVALS_DECIDE => decide_run_approvals(RunApprovalsDecideParams) -> RunApprovalsDecideResponse =>
        ["Decide pending run approvals", "Approves or rejects pending MCP tool calls on the named active run. Valid decisions apply independently; the run resumes only after every pending approval has a decision."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_SESSION_RUNS_STEER => steer_run(RunSteerParams) -> RunSteerResponse =>
        ["Steer the active run", "Injects input into the named active run; the model sees it at the next turn boundary without interrupting the in-flight turn. Accepted while the run is running or parked on an await; rejected for queued, cancelling, or finished runs."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_SESSION_SKILLS_LIST => list_skills(SkillListParams) -> SkillListResponse =>
        ["List available session skills", "Returns separate VFS and environment catalogs with source, reference, availability, readable skill paths, and warnings. Refreshes only when open with no active or queued run, without waking environments. Absent catalogs are omitted."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_SESSION_PROFILES_APPLY => apply_profile(ProfileApplyParams) -> ProfileApplyResponse =>
        ["Apply a profile to a session", "Applies a named or inline profile's config, instructions, and environment setup to an existing session; mutating profile sections require it to be open and idle. Pass current revisions to guard concurrent changes."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_SESSION_ENVIRONMENTS_ACTIVATE => activate_session_environment(SessionEnvironmentActivateParams) -> SessionEnvironmentActivateResponse =>
        ["Activate a session environment", "Selects an attached, live universe environment for environment-targeted tools while the session is idle."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_SESSION_ENVIRONMENTS_DEACTIVATE => deactivate_session_environment(SessionEnvironmentDeactivateParams) -> SessionEnvironmentDeactivateResponse =>
        ["Deactivate the session environment", "Clears active environment selection without changing or closing the universe environment."], access: MethodAccess::Universe(UniverseAction::ControlSession),
    METHOD_ENVIRONMENTS_CREDENTIALS_BIND => bind_environment_credential(EnvironmentCredentialBindParams) -> EnvironmentCredentialBindResponse =>
        ["Bind a credential into an environment", "Maps an environment variable name to an existing grant/provider/direct-secret handle for a universe environment. Requires configuring the environment and configuring resources in the universe. The response exposes only the source handle, never secret material."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_ENVIRONMENTS_CREDENTIALS_LIST => list_environment_credentials(EnvironmentCredentialListParams) -> EnvironmentCredentialListResponse =>
        ["List environment credential bindings", "Returns variable names and credential source handles for a universe environment; resolved secret values are never returned."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_ENVIRONMENTS_CREDENTIALS_UNBIND => unbind_environment_credential(EnvironmentCredentialUnbindParams) -> EnvironmentCredentialUnbindResponse =>
        ["Unbind an environment credential", "Removes one variable-to-credential mapping without deleting the underlying grant, provider credential, or secret."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_ENVIRONMENTS_CREATE => create_environment(EnvironmentCreateParams) -> EnvironmentCreateResponse =>
        ["Create an environment", "Records an idempotent provisioning intent against an enabled universe binding, attributed to the caller. The provider validates its provider-wide template and provisions through its backend asynchronously."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_ENVIRONMENTS_READ => read_environment(EnvironmentReadParams) -> EnvironmentReadResponse =>
        ["Read an environment", "Returns the durable universe resource, source binding, logical lifecycle state, and minimal current-incarnation identity."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_ENVIRONMENTS_LIST => list_environments(EnvironmentListParams) -> EnvironmentListResponse =>
        ["List environments", "Lists the universe environments, optionally filtered by provider, binding, or logical lifecycle state."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_ENVIRONMENTS_CLOSE => close_environment(EnvironmentCloseParams) -> EnvironmentCloseResponse =>
        ["Close an environment", "Records an asynchronous idempotent close intent. Provider cleanup is resumed by lifecycle reconciliation; quota is released only after Closed."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_ENVIRONMENTS_EXTERNAL_CREATE => create_external_environment(EnvironmentExternalCreateParams) -> EnvironmentExternalCreateResponse =>
        ["Register an external environment", "Creates an environment backed by a Lightspeed-reachable envd WebSocket endpoint, attributed to the caller. Reachability is checked on demand."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_ENVIRONMENTS_INGRESS_PUT => put_environment_ingress(EnvironmentIngressPutParams) -> EnvironmentIngressPutResponse =>
        ["Configure environment public ingress", "Synchronously enables or disables one provider-authorized HTTPS endpoint for a provisioned environment. The provider owns hostname allocation, the approved guest port, routing, TLS, and health."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_ENVIRONMENTS_POWER_PUT => put_environment_power(EnvironmentPowerPutParams) -> EnvironmentPowerPutResponse =>
        ["Set environment power intent", "Records the desired power state (running, paused, suspended, or stopped) of a provisioned environment; the lifecycle reconciler converges the provider target asynchronously. Powered-down environments wake transparently on their next use. Rejected when the provider does not support the state."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_ENVIRONMENTS_IDLE_POLICY_PUT => put_environment_idle_policy(EnvironmentIdlePolicyPutParams) -> EnvironmentIdlePolicyPutResponse =>
        ["Set environment idle policy", "Replaces or clears the staged idle policy of a provisioned environment. The power reaper measures the daemon's idle duration against the pause/suspend/stop/close thresholds and escalates through the stages the provider supports."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_ENVIRONMENTS_PROVIDER_BINDINGS_LIST => list_environment_provider_bindings(EnvironmentProviderBindingListParams) -> EnvironmentProviderBindingListResponse =>
        ["List environment provider bindings", "Lists this universe's revisioned routing and admission bindings to deployment-scoped physical providers."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_ENVIRONMENTS_PROVIDER_BINDINGS_READ => read_environment_provider_binding(EnvironmentProviderBindingReadParams) -> EnvironmentProviderBindingReadResponse =>
        ["Read an environment provider binding", "Returns one universe routing and admission binding. Provider-wide templates and physical resource, network, and ingress policy remain provider-owned."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_ENVIRONMENTS_TEMPLATES_LIST => list_environment_templates(EnvironmentTemplateListParams) -> EnvironmentTemplateListResponse =>
        ["List environment templates", "Reads immutable templates directly from the selected bound provider controller."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_ENVIRONMENTS_TEMPLATES_READ => read_environment_template(EnvironmentTemplateReadParams) -> EnvironmentTemplateReadResponse =>
        ["Read an environment template", "Returns one immutable template version from the selected bound provider controller."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_ENVIRONMENTS_JOBS_CREATE => create_environment_jobs(EnvironmentJobCreateParams) -> EnvironmentJobCreateResponse =>
        ["Create environment jobs", "Starts a dependency-aware job group on one environment instance, injecting the environment's configured credentials at provider start. requestId is the retry identity; jobs are owned by the instance rather than a session. A powered-down environment is woken on use: the call fails with environment_not_ready while the wake is in progress; retry with backoff."], access: MethodAccess::Universe(UniverseAction::UseResource),
    METHOD_ENVIRONMENTS_JOBS_READ => read_environment_jobs(EnvironmentJobReadParams) -> EnvironmentJobReadResponse =>
        ["Read environment jobs", "Reads selected job handles with bounded output, optional sequence continuation, and optional artifacts; use returned status/sequence data for polling."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_ENVIRONMENTS_JOBS_CANCEL => cancel_environment_jobs(EnvironmentJobCancelParams) -> EnvironmentJobCancelResponse =>
        ["Cancel environment jobs", "Requests cancellation for selected jobs, optionally including dependents. Force is provider-specific escalation; inspect each per-job result."], access: MethodAccess::Universe(UniverseAction::UseResource),
    METHOD_ENVIRONMENTS_REGISTRATION_KEYS_CREATE => create_environment_registration_key(EnvironmentRegistrationKeyCreateParams) -> EnvironmentRegistrationKeyCreateResponse =>
        ["Mint an environment registration key", "Creates a reusable universe-scoped key that lets outbound envd daemons register as environments. The plaintext secret is returned exactly once; only its hash is stored. Identity mode, active limit, disconnect grace, and expiry are the key's policy. Treat the secret like a cluster-join credential."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_ENVIRONMENTS_REGISTRATION_KEYS_READ => read_environment_registration_key(EnvironmentRegistrationKeyReadParams) -> EnvironmentRegistrationKeyReadResponse =>
        ["Read an environment registration key", "Returns the key's display prefix, policy, status, and derived environment counts; never the secret or its hash."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_ENVIRONMENTS_REGISTRATION_KEYS_LIST => list_environment_registration_keys(EnvironmentRegistrationKeyListParams) -> EnvironmentRegistrationKeyListResponse =>
        ["List environment registration keys", "Lists this universe's registration keys with policy, status, and derived counts. Each key is the group of the environments it admitted."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_ENVIRONMENTS_REGISTRATION_KEYS_REVOKE => revoke_environment_registration_key(EnvironmentRegistrationKeyRevokeParams) -> EnvironmentRegistrationKeyRevokeResponse =>
        ["Revoke an environment registration key", "Stops the key from admitting new daemon identities; already registered daemons keep reconnecting. With closeEnvironments, also closes every non-closed environment the key admitted. Idempotent."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_MODELS_LIST => list_models(ModelListParams) -> ModelListResponse =>
        ["Discover available models", "Queries supported providers directly, with a brief process-local burst cache, and returns best-effort selectable routes. One provider failure does not discard successful results from others."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_PROFILES_CREATE => create_profile(ProfileCreateParams) -> ProfileCreateResponse =>
        ["Create an agent profile", "Creates a new universe-scoped reusable profile document; use profiles/put for create-or-replace revision semantics."], access: MethodAccess::Universe(UniverseAction::CreateProfile),
    METHOD_PROFILES_READ => read_profile(ProfileReadParams) -> ProfileReadResponse =>
        ["Read an agent profile", "Returns the complete profile document and current revision."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_PROFILES_LIST => list_profiles(ProfileListParams) -> ProfileListResponse =>
        ["List agent profiles", "Returns lightweight summaries of universe-scoped reusable profiles."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_PROFILES_PUT => put_profile(ProfilePutParams) -> ProfilePutResponse =>
        ["Create or replace an agent profile", "Stores the complete profile document. Use expectedRevision from profiles/read when replacing to prevent lost updates; absence writes unconditionally."], access: MethodAccess::Universe(UniverseAction::ManageProfile),
    METHOD_PROFILES_DELETE => delete_profile(ProfileDeleteParams) -> ProfileDeleteResponse =>
        ["Delete an agent profile", "Deletes the catalog document; sessions previously created or configured from it retain their materialized state."], access: MethodAccess::Universe(UniverseAction::ManageProfile),
    METHOD_BLOBS_PUT => put_blobs(BlobPutParams) -> BlobPutResponse =>
        ["Store content-addressed blobs", "Decodes and stores a batch of base64 payloads, returning immutable content references in request order. Re-uploading identical bytes is naturally deduplicated."], access: MethodAccess::Universe(UniverseAction::UseResource),
    METHOD_BLOBS_READ => read_blob(BlobReadParams) -> BlobReadResponse =>
        ["Read a content-addressed blob", "Returns the complete immutable blob of this universe as base64; large values count against gateway and MCP response limits."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_BLOBS_HAS => has_blobs(BlobHasParams) -> BlobHasResponse =>
        ["Check blob availability", "Checks a batch of content references without returning blob bodies, preserving request order."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_VFS_SNAPSHOTS_COMMIT => commit_vfs_snapshot(VfsSnapshotCommitParams) -> VfsSnapshotCommitResponse =>
        ["Commit a VFS snapshot", "Validates and stores an immutable filesystem manifest. Upload referenced file blobs first; the returned snapshot ref is content-addressed."], access: MethodAccess::Universe(UniverseAction::UseResource),
    METHOD_VFS_SNAPSHOTS_READ => read_vfs_snapshot(VfsSnapshotReadParams) -> VfsSnapshotReadResponse =>
        ["Read a VFS snapshot", "Returns an immutable snapshot manifest and aggregate file/byte counts; file bodies remain separate blobs."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_VFS_WORKSPACES_CREATE => create_vfs_workspace(VfsWorkspaceCreateParams) -> VfsWorkspaceCreateResponse =>
        ["Create a mutable VFS workspace", "Creates a universe workspace attributed to the caller at an optional seed snapshot; absence starts from a server-created empty snapshot."], access: MethodAccess::Universe(UniverseAction::CreateWorkspace),
    METHOD_VFS_WORKSPACES_READ => read_vfs_workspace(VfsWorkspaceReadParams) -> VfsWorkspaceReadResponse =>
        ["Read a VFS workspace", "Returns workspace metadata, current head snapshot, and revision for safe updates."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_VFS_WORKSPACES_LIST => list_vfs_workspaces(VfsWorkspaceListParams) -> VfsWorkspaceListResponse =>
        ["List VFS workspaces", "Lists the mutable universe workspaces with head snapshots, sizes, and revisions."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_VFS_WORKSPACES_UPDATE => update_vfs_workspace(VfsWorkspaceUpdateParams) -> VfsWorkspaceUpdateResponse =>
        ["Update a VFS workspace", "Moves the workspace head to an existing snapshot, which requires use of the workspace, and updates its display name, which requires configuring it. Pass expectedRevision from a read to prevent lost updates."], access: MethodAccess::Universe(UniverseAction::UseResource),
    METHOD_VFS_WORKSPACES_DELETE => delete_vfs_workspace(VfsWorkspaceDeleteParams) -> VfsWorkspaceDeleteResponse =>
        ["Delete a VFS workspace", "Deletes the mutable workspace record; immutable snapshots and blobs remain content-addressed resources."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_MCP_SERVERS_PUT => put_mcp_server(McpServerPutParams) -> McpServerPutResponse =>
        ["Create or replace an MCP server record", "Stores the complete catalog document with its optional auth-grant credential. A new server is attributed to the caller. Use expectedRevision when replacing; token material is never accepted or returned."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_MCP_SERVERS_AUTH_DISCOVER => discover_mcp_server_auth(McpServerAuthDiscoverParams) -> McpServerAuthDiscoverResponse =>
        ["Discover MCP server authentication", "Looks for standards-based OAuth protected-resource metadata without creating a server, OAuth client, flow, or grant. An absent OAuth result is inconclusive and callers must allow manual auth selection."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_MCP_SERVERS_TOOLS_DISCOVER => discover_mcp_server_tools(McpServerToolsDiscoverParams) -> McpServerToolsDiscoverResponse =>
        ["Discover MCP server tools", "Connects directly to the configured MCP server with its current universe credential and returns one bounded live tools/list result. The inventory is never persisted or cached and no tool is invoked."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_MCP_SERVERS_READ => read_mcp_server(McpServerReadParams) -> McpServerReadResponse =>
        ["Read an MCP server record", "Returns one catalog document with defaults, auth policy, non-secret grant binding, status, and revision; no credential value is exposed."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_MCP_SERVERS_LIST => list_mcp_servers(McpServerListParams) -> McpServerListResponse =>
        ["List MCP server records", "Lists the universe catalog entries, optionally filtered by lifecycle/configuration status."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_MCP_SERVERS_DELETE => delete_mcp_server(McpServerDeleteParams) -> McpServerDeleteResponse =>
        ["Delete an MCP server record", "Deletes the catalog document. Existing session configs that reference it are not silently rewritten and may need explicit reconfiguration."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_AUTH_GRANTS_IMPORT => import_auth_grant(AuthGrantImportParams) -> AuthGrantImportResponse =>
        ["Import a static bearer grant", "Accepts a plaintext token, encrypts it immediately, and returns only grant metadata/token-presence flags. Brokered is the default; retrievable exposure is immutable and permits service-only leases."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_AUTH_GRANTS_LEASE => lease_auth_grant(AuthGrantLeaseParams) -> AuthGrantLeaseResponse =>
        ["Lease a retrievable authentication grant", "Service callers only. Resolves the current access token through the broker, records the lease, and returns it once. Cache only in memory until expiry minus margin (or at most five minutes without expiry), re-lease after target 401/403, and never persist or place the token in workflow payloads."], access: MethodAccess::Service,
    METHOD_AUTH_GRANTS_READ => read_auth_grant(AuthGrantReadParams) -> AuthGrantReadResponse =>
        ["Read authentication grant metadata", "Returns creator attribution, provider binding, scopes, audience, expiry, status, and token-presence flags; access and refresh token values are never returned."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_AUTH_GRANTS_LIST => list_auth_grants(AuthGrantListParams) -> AuthGrantListResponse =>
        ["List authentication grants", "Lists non-secret grant metadata for the universe, optionally filtered by status."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_AUTH_GRANTS_REVOKE => revoke_auth_grant(AuthGrantRevokeParams) -> AuthGrantRevokeResponse =>
        ["Revoke an authentication grant", "Marks the grant unusable by token consumers while retaining non-secret audit metadata."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_AUTH_CLIENTS_CREATE => create_auth_client(AuthClientCreateParams) -> AuthClientCreateResponse =>
        ["Register an OAuth client", "Stores provider endpoints and client identity; an optional plaintext client secret is encrypted and represented thereafter only by hasClientSecret."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_AUTH_CLIENTS_READ => read_auth_client(AuthClientReadParams) -> AuthClientReadResponse =>
        ["Read OAuth client metadata", "Returns endpoints, public client identity, defaults, and secret-presence state; the client secret is never returned."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_AUTH_CLIENTS_LIST => list_auth_clients(AuthClientListParams) -> AuthClientListResponse =>
        ["List OAuth clients", "Lists non-secret OAuth client registrations in the universe."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_AUTH_CLIENTS_DELETE => delete_auth_client(AuthClientDeleteParams) -> AuthClientDeleteResponse =>
        ["Delete an OAuth client", "Deletes the client registration and its stored client secret; grants already created from it remain separate records."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_AUTH_FLOWS_START => start_auth_flow(AuthFlowStartParams) -> AuthFlowStartResponse =>
        ["Start an OAuth authorization flow", "Creates a short-lived PKCE flow carrying the immutable grant exposure choice and returns a browser authorization URL containing one-time state. Treat the URL as sensitive and poll auth/flows/read for completion."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_AUTH_FLOWS_READ => read_auth_flow_status(AuthFlowStatusParams) -> AuthFlowStatusResponse =>
        ["Read OAuth flow status", "Polls a flow's pending/completed/failed/expired state and returns the resulting grant id when authorization succeeds; no token value is exposed."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_AUTH_PROVIDERS_CREATE => create_auth_provider(AuthProviderCreateParams) -> AuthProviderCreateResponse =>
        ["Register an authentication provider", "Creates a model or GitHub credential source. Plaintext API keys/private keys are encrypted on receipt and later represented only by configuration plus hasCredential."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_AUTH_PROVIDERS_READ => read_auth_provider(AuthProviderReadParams) -> AuthProviderReadResponse =>
        ["Read authentication provider metadata", "Returns provider kind, non-secret configuration, credential-presence state, and status; stored credentials are never returned."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_AUTH_PROVIDERS_LIST => list_auth_providers(AuthProviderListParams) -> AuthProviderListResponse =>
        ["List authentication providers", "Lists non-secret model/GitHub provider registrations for the universe."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_AUTH_PROVIDERS_DELETE => delete_auth_provider(AuthProviderDeleteParams) -> AuthProviderDeleteResponse =>
        ["Delete an authentication provider", "Deletes the provider registration and its directly stored credential; separately stored grants remain independent records."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_AUTH_GITHUB_INSTALLATIONS_LIST => list_github_installations(AuthGitHubInstallationListParams) -> AuthGitHubInstallationListResponse =>
        ["List GitHub App installations", "Uses the registered GitHub App provider credential to query accessible installations and returns account/permission metadata without tokens."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_AUTH_GITHUB_INSTALLATIONS_GRANT => grant_github_installation(AuthGitHubInstallationGrantParams) -> AuthGitHubInstallationGrantResponse =>
        ["Grant access to a GitHub App installation", "Creates or refreshes a universe auth grant for one accessible installation. The installation token is brokered internally and never returned."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_BOTS_CREATE => create_bot(BotCreateParams) -> BotCreateResponse =>
        ["Create a bot", "Creates the bot record, optionally with its triggers, and starts its controller. Fails if the bot id exists; a trigger failure rolls the bot back."], access: MethodAccess::Universe(UniverseAction::CreateBot),
    METHOD_BOTS_PUT => put_bot(BotPutParams) -> BotPutResponse =>
        ["Create or replace a bot document", "Replaces the mutable configuration whole and signals the controller, which applies it at its next idle boundary. Pass expectedRevision when replacing; a closed bot accepts label-only edits."], access: MethodAccess::Universe(UniverseAction::ManageBot),
    METHOD_BOTS_READ => read_bot(BotReadParams) -> BotReadResponse =>
        ["Read a bot", "Returns the bot record, its current revision, and lifecycle columns."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_BOTS_LIST => list_bots(BotListParams) -> BotListResponse =>
        ["List bots", "Returns the roster: every bot with its trigger count, pending event count, and latest event, optionally narrowed by createdBy."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_BOTS_CLOSE => close_bot(BotCloseParams) -> BotCloseResponse =>
        ["Close a bot", "Terminal and idempotent: disables every trigger, drops schedules, and tells the controller to archive pending events and force-close its sessions. Returns once signalled; follow bots/state/read for closing to closed."], access: MethodAccess::Universe(UniverseAction::ManageBot),
    METHOD_BOTS_DELETE => delete_bot(BotDeleteParams) -> BotDeleteResponse =>
        ["Delete a bot", "Closes the bot if needed, waits for its controller to complete, deletes the sessions it closed, and removes the record so the bot id is free again."], access: MethodAccess::Universe(UniverseAction::ManageBot),
    METHOD_BOTS_STATE_READ => read_bot_state(BotStateReadParams) -> BotStateReadResponse =>
        ["Read bot controller state", "Queries the controller workflow for its live snapshot (sessions, buffers, active and recent deliveries, budget) and lists sub-agent descendants. The controller is absent until the bot's first event."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_BOTS_SESSIONS_ROTATE => rotate_bot_session(BotSessionRotateParams) -> BotSessionRotateResponse =>
        ["Rotate a bot session", "Asks the controller to close one of the bot's sessions at its next idle boundary and continue on a fresh generation; queued deliveries follow."], access: MethodAccess::Universe(UniverseAction::ManageBot),
    METHOD_BOTS_TRIGGERS_PUT => put_bot_trigger(BotTriggerPutParams) -> BotTriggerPutResponse =>
        ["Create or replace a trigger", "Validates the trigger document (CEL parses, grants exist, one inbox per bot, chat routes per conversation), reconciles its Temporal Schedule, and stores it. A poll spec edit resets the cursor; a webhook keeps its URL token."], access: MethodAccess::Universe(UniverseAction::ManageBot),
    METHOD_BOTS_TRIGGERS_READ => read_bot_trigger(BotTriggerReadParams) -> BotTriggerReadResponse =>
        ["Read a trigger", "Returns one trigger with its incidents and cursor; the ingest path and pairing code are included for bot-management callers."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_BOTS_TRIGGERS_LIST => list_bot_triggers(BotTriggerListParams) -> BotTriggerListResponse =>
        ["List a bot's triggers", "Returns every trigger of the bot ordered by id, secrets included for bot-management callers."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_BOTS_TRIGGERS_DELETE => delete_bot_trigger(BotTriggerDeleteParams) -> BotTriggerDeleteResponse =>
        ["Delete a trigger", "Drops the trigger's Temporal Schedule and pairings, then the record; stored events keep their history."], access: MethodAccess::Universe(UniverseAction::ManageBot),
    METHOD_BOTS_EVENTS_ADMIT => admit_bot_event(BotEventAdmitParams) -> BotEventAdmitResponse =>
        ["Admit an event manually", "Stores an operator-authored event for the bot's main session and wakes the controller. eventId is the dedupe identity; a duplicate returns the stored row."], access: MethodAccess::Universe(UniverseAction::InvokeBot),
    METHOD_BOTS_EVENTS_REPLAY => replay_bot_event(BotEventReplayParams) -> BotEventReplayResponse =>
        ["Replay a stored event", "Re-admits the stored envelope as a fresh event with the original routing; the replay never coalesces."], access: MethodAccess::Universe(UniverseAction::ManageBot),
    METHOD_BOTS_EVENTS_LIST => list_bot_events(BotEventListParams) -> BotEventListResponse =>
        ["List a bot's events", "Cursor-paginated event log, newest first, with outcomes; payload documents stay in the CAS."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_BOTS_EVENTS_READ => read_bot_event(BotEventReadParams) -> BotEventReadResponse =>
        ["Read an event by number", "Returns the event row and its full stored envelope document."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_BOTS_FILTERS_TEST => test_bot_filter(BotFilterTestParams) -> BotFilterTestResponse =>
        ["Test a CEL filter", "Evaluates a filter against one payload or a sample of recent stored events, reporting matches and evaluation errors without changing anything."], access: MethodAccess::Universe(UniverseAction::ManageBot),
    METHOD_CHANNELS_ACCOUNTS_CREATE => create_channel_account(ChannelAccountCreateParams) -> ChannelAccountCreateResponse =>
        ["Create a channel account", "Registers a provider account (Telegram, WhatsApp) for this universe. The credential is a retrievable grant reference; no token is accepted here."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_CHANNELS_ACCOUNTS_PUT => put_channel_account(ChannelAccountPutParams) -> ChannelAccountPutResponse =>
        ["Create or replace a channel account", "Replaces the account document whole; pass expectedRevision when replacing. The connector host picks the change up on its next discovery pass."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_CHANNELS_ACCOUNTS_READ => read_channel_account(ChannelAccountReadParams) -> ChannelAccountReadResponse =>
        ["Read a channel account", "Returns the account document and revision."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_CHANNELS_ACCOUNTS_LIST => list_channel_accounts(ChannelAccountListParams) -> ChannelAccountListResponse =>
        ["List channel accounts", "Lists this universe's provider accounts, optionally by provider."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_CHANNELS_ACCOUNTS_DELETE => delete_channel_account(ChannelAccountDeleteParams) -> ChannelAccountDeleteResponse =>
        ["Delete a channel account", "Removes the account and its pairings; chat triggers that reference it stop serving conversations."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_CHANNELS_INBOUND_ADMIT => admit_channel_inbound(ChannelInboundAdmitParams) -> ChannelInboundAdmitResponse =>
        ["Admit a provider message", "Service callers only. Resolves the chat trigger for the conversation, applies pairing, and signals the conversation workflow. Returns the decision so the connector can send pairing prompts itself; acknowledge the provider only after this returns."], access: MethodAccess::Service,
    METHOD_CHANNELS_PAIRINGS_LIST => list_channel_pairings(ChannelPairingListParams) -> ChannelPairingListResponse =>
        ["List chat pairings", "Lists conversations paired to chat triggers, optionally by account or bot."], access: MethodAccess::Universe(UniverseAction::Read),
    METHOD_CHANNELS_PAIRINGS_DELETE => delete_channel_pairing(ChannelPairingDeleteParams) -> ChannelPairingDeleteResponse =>
        ["Unpair a conversation", "Removes one pairing; the conversation must present the pairing code again to reconnect."], access: MethodAccess::Universe(UniverseAction::ConfigureResource),
    METHOD_CHANNELS_CONVERSATIONS_READ => read_channel_conversation(ChannelConversationReadParams) -> ChannelConversationReadResponse =>
        ["Read a conversation snapshot", "Queries the conversation workflow's live state for one chat, for debugging; absent when no workflow exists yet."], access: MethodAccess::Universe(UniverseAction::Read),
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
