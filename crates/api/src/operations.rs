use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentApiErrorKind {
    InvalidRequest,
    NotFound,
    /// This universe was permanently deleted; the error carries its record.
    Gone,
    Conflict,
    /// The request was understood and the caller may make it, but current
    /// state or policy refuses it. Never an authorization decision.
    Rejected,
    /// No valid credential was presented. An authenticated gateway decides
    /// this from the request headers and answers HTTP 401 without reading
    /// the request body.
    Unauthenticated,
    /// The authenticated caller lacks permission for this operation or target.
    Forbidden,
    /// No model was supplied and this universe has no default for the requested use.
    ModelDefaultUnset,
    AudioBlobTooLarge,
    /// The session's agent workflow exists but failed during bootstrap
    /// (rehydration) and cannot serve runs. Distinct from `NotFound` (no
    /// workflow) so clients/bridges treat it as a session recovery problem
    /// rather than an ordinary "answer this message" failure.
    SessionBootstrapFailed,
    /// The session is open and its history is intact, but its workflow execution
    /// has stopped, so nothing can serve it. Nothing restarts it behind the caller:
    /// `session/restart`, or a retried `session/start` with its id, brings it back.
    SessionExecutionStopped,
    /// The environment exists but is not reachable yet: it is still
    /// provisioning, its provider is waking or booting it after a data
    /// route dial, or its registered daemon is reconnecting. Distinct from
    /// `Rejected` so clients retry with backoff instead of failing —
    /// polling and automation callers lean on this for wake-on-use.
    EnvironmentNotReady,
    /// The requested page still exceeds the public serialized response
    /// budget. Callers can retry with a smaller page limit or continuation.
    ResponseTooLarge,
    /// The wait for this command expired while the session may still be holding it,
    /// for example behind a compaction. It can still take effect, so this is not a
    /// failure. Retrying the same request, with the same submission id where it takes
    /// one, is safe.
    OperationPending,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Error)]
#[error("{kind:?}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct AgentApiError {
    pub kind: AgentApiErrorKind,
    pub message: String,
    /// Present for modelDefaultUnset; clients need not parse the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_default_slot: Option<ModelDefaultSlot>,
    /// Present for operationPending on a call that took a submission id, including
    /// one the server allocated. Retrying with it cannot land the same work twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_universe: Option<Box<DeletedUniverseView>>,
}

impl AgentApiError {
    pub fn new(kind: AgentApiErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            model_default_slot: None,
            submission_id: None,
            deleted_universe: None,
        }
    }

    pub fn model_default_unset(slot: ModelDefaultSlot) -> Self {
        Self {
            kind: AgentApiErrorKind::ModelDefaultUnset,
            message: format!(
                "no universe model default is configured for {}; choose a model or set it with models/defaults/put",
                slot.as_str()
            ),
            model_default_slot: Some(slot),
            submission_id: None,
            deleted_universe: None,
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::InvalidRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::NotFound, message)
    }

    pub fn gone(record: DeletedUniverseView) -> Self {
        let mut error = Self::new(AgentApiErrorKind::Gone, "universe was permanently deleted");
        error.deleted_universe = Some(Box::new(record));
        error
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

    pub fn audio_blob_too_large(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::AudioBlobTooLarge, message)
    }

    pub fn session_bootstrap_failed(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::SessionBootstrapFailed, message)
    }

    pub fn session_execution_stopped(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::SessionExecutionStopped, message)
    }

    pub fn environment_not_ready(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::EnvironmentNotReady, message)
    }

    pub fn response_too_large(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::ResponseTooLarge, message)
    }

    pub fn operation_pending(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::OperationPending, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::Internal, message)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodScope {
    Connection,
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
            Self::Connection => "connection",
            Self::Universe => "universe",
            Self::Service => "service",
            Self::Deployment => "deployment",
        }
    }
}

/// One typed operation, including its resource HTTP binding and DTO schemas. The same exported declaration generates this metadata and the
/// runtime's typed handlers, so routes and service bindings cannot drift.
pub struct MethodSpec {
    /// Nonempty for streaming methods; these notifications precede the final response.
    pub notifications: &'static [&'static str],
    pub method: &'static str,
    pub scope: MethodScope,
    pub access: MethodAccess,
    /// Concise operation label suitable for API indexes and generated JSDoc.
    pub summary: &'static str,
    /// Short operational guidance: lifecycle, concurrency, security, and
    /// prerequisite semantics that are not obvious from the parameter schema.
    pub description: &'static str,
    pub params_type: &'static str,
    pub result_type: String,
    pub http: HttpBinding,
    pub register_schemas: fn(&mut schemars::SchemaGenerator) -> MethodSchemas,
}

pub struct MethodSchemas {
    pub params: schemars::Schema,
    pub result: schemars::Schema,
}

macro_rules! stream_notifications {
    () => {
        &[]
    };
    ($value:expr) => {
        $value
    };
}

macro_rules! api_methods {
    ($($method_const:path => $service_fn:ident($params:ty) -> $response:ty =>
        [$summary:expr, $description:expr], $(streaming: $emits:expr,)? access: $access:expr,
        http: ($verb:ident, $path:literal, [$(($parameter:literal, $field:literal)),*], [$($query:literal),*], $transfer:ident, $success:ident)),+ $(,)?) => {
        pub(crate) fn universe_method_access(method: &str) -> Option<MethodAccess> {
            match method {
                $($method_const => Some($access),)+
                _ => None,
            }
        }


        /// One entry per operation in canonical declaration order.
        pub fn method_manifest() -> Vec<MethodSpec> {
            vec![
                $(
                    MethodSpec {
                        method: $method_const,
                        notifications: stream_notifications!($($emits)?),
                        scope: ($access).scope(),
                        access: $access,
                        summary: $summary,
                        description: $description,
                        params_type: crate::http_contract::canonical_type_name::<$params>(),
                        result_type: crate::http_contract::canonical_type_name::<$response>().into(),
                        http: HttpBinding { verb: HttpVerb::$verb, path: $path, path_fields: &[$(HttpPathField { parameter: $parameter, field: $field }),*], query_fields: &[$($query),*], transfer: HttpTransfer::$transfer, success: HttpSuccess::$success },
                        register_schemas: |generator| MethodSchemas {
                            params: generator.subschema_for::<$params>(),
                            result: generator.subschema_for::<$response>(),
                        },
                    },
                )+
            ]
        }
    };
}

/// Invoke a consumer with the canonical typed operations and their HTTP bindings.
#[macro_export]
macro_rules! for_each_universe_operation {
    ($consumer:ident) => {
        $consumer! {
    $crate::METHOD_UNIVERSE_WATCH => watch_universe($crate::UniverseWatchParams) -> () =>
        ["Watch universe list changes", "Requires Accept: text/event-stream. Emits list rows after a short batching window, starting with watch/reset. Refetch on reset and periodically: signals can be lost. Ends after 15 minutes; reconnect to reauthenticate. Includes sub-agents; visibleTo restricts session access as on session/list."], streaming: $crate::WATCH_NOTIFICATIONS, access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/events", [], ["kinds", "visibleTo"], Sse, Ok),
    $crate::METHOD_VFS_WORKSPACES_FILES_READ => read_vfs_workspace_file($crate::VfsWorkspaceFileReadParams) -> Vec<u8> =>
        ["Read a workspace file", "Reads bytes at a path in the current workspace head."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/vfs/workspaces/{workspaceId}/files/content", [("workspaceId", "workspaceId")], ["path"], BinaryDownload, Ok),
    $crate::METHOD_INITIALIZE => initialize($crate::ConnectionParams) -> $crate::ConnectionResponse =>
        ["Inspect the runtime connection", "Returns server identity, HTTP contract revision, token attribution and the default universe when configured. Requires authentication in authenticated mode. Universe selection belongs in resource URLs."], access: $crate::MethodAccess::Connection,
        http: (GET, "/api/v1/connection", [], [], Json, Ok),
    $crate::METHOD_SESSION_START => start_session($crate::SessionStartParams) -> $crate::SessionStartResponse =>
        ["Start a session", "Creates a session, unshared unless access says universe, with optional config, profile and immutable integration. Explicit values override profile metadata and retention; the default environment attachment becomes active. Retrying an id returns it and restarts a stopped execution from its log, unless integrated. Failed setup closes a server-allocated id."], access: $crate::MethodAccess::Universe($crate::UniverseAction::CreateSession),
        http: (POST, "/api/v1/universes/{universe}/sessions", [], [], Json, Ok),
    $crate::METHOD_SESSION_READ => read_session($crate::SessionReadParams) -> $crate::SessionReadResponse =>
        ["Read a session", "Returns stored state plus a bounded newest-first run-summary page, and needs no worker. An open session whose execution has stopped reports executionFailure. Follow nextRunCursor with session/runs/list when hasOlderRuns is true; use session/events/read for the transcript."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/sessions/{sessionId}", [("sessionId", "sessionId")], ["runLimit"], Json, Ok),
    $crate::METHOD_SESSION_LIST => list_sessions($crate::SessionListParams) -> $crate::SessionListResponse =>
        ["List sessions", "Returns a cursor-paginated summary list ordered by most recent update, optionally narrowed by filters such as visibleTo, which keeps sessions whose root is shared with the universe or was created by that actor. An unknown parameter is refused. Pages may shift while sessions are changing."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/sessions", [], ["closed", "cursor", "lifecycleControlled", "limit", "metadata", "parent", "subagent", "trees", "visibleTo"], Json, Ok),
    $crate::METHOD_SESSION_CONFIG_PUT => put_session_config($crate::SessionConfigPutParams) -> $crate::SessionConfigPutResponse =>
        ["Replace session configuration", "Replaces the complete sparse config while the session is idle. Use the current config revision for safe read-modify-write; omitted features are revoked, an omitted model preserves the current model, and an identical document is a no-op. A newly attached MCP server must be usable; a held one that has gone away only drops out of later runs."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureSession),
        http: (PUT, "/api/v1/universes/{universe}/sessions/{sessionId}/config", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_RENAME => rename_session($crate::SessionRenameParams) -> $crate::SessionRenameResponse =>
        ["Rename a session", "Sets the display name, or clears it when displayName is omitted."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ControlSession),
        http: (PUT, "/api/v1/universes/{universe}/sessions/{sessionId}/name", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_METADATA_PUT => put_session_metadata($crate::SessionMetadataPutParams) -> $crate::SessionMetadataPutResponse =>
        ["Replace session metadata", "Replaces the complete descriptive key/value map (bounded like session/start); an omitted or empty map clears it. Record-only: the event log and updatedAtMs are untouched."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureSession),
        http: (PUT, "/api/v1/universes/{universe}/sessions/{sessionId}/metadata", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_RETENTION_PUT => put_session_retention($crate::SessionRetentionPutParams) -> $crate::SessionRetentionPutResponse =>
        ["Replace session retention", "Sets the positive close-relative automatic soft-deletion duration on a retention root, or clears it with null. Delegated children inherit the root policy and cannot override it. Retention does not permanently delete stored history."], access: $crate::MethodAccess::Universe($crate::UniverseAction::SetSessionRetention),
        http: (PUT, "/api/v1/universes/{universe}/sessions/{sessionId}/retention", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_RESTART => restart_session($crate::SessionRestartParams) -> $crate::SessionRestartResponse =>
        ["Restart a stopped session", "Starts a new workflow execution for an open session whose last one stopped, from its log. A run that was not parked fails, a parked run keeps waiting, and queued runs start afterwards. A running session is left alone. Refused for a closed session and for one with no run history."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureSession),
        http: (POST, "/api/v1/universes/{universe}/sessions/{sessionId}/restart", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_CLOSE => close_session($crate::SessionCloseParams) -> $crate::SessionCloseResponse =>
        ["Close a session", "Closes an idle session and detaches its environment bindings. Without force, active work of any kind, including a pending detached promise, rejects the call. Force mode cancels active work, drops queued runs, and can recover a session whose workflow is unavailable."], access: $crate::MethodAccess::Universe($crate::UniverseAction::CloseSession),
        http: (POST, "/api/v1/universes/{universe}/sessions/{sessionId}/close", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_DELETE => delete_session($crate::SessionDeleteParams) -> $crate::SessionDeleteResponse =>
        ["Delete closed sessions", "Permanently deletes a closed retention-tree leaf, or its closed delegated-child subtree when cascade is true. Set softDelete to true to hide sessions while retaining records and history until a deployment administrator purges them. Prior soft deletion is not required for permanent deletion."], access: $crate::MethodAccess::Universe($crate::UniverseAction::DeleteSession),
        http: (DELETE, "/api/v1/universes/{universe}/sessions/{sessionId}", [("sessionId", "sessionId")], ["cascade", "sharedOnly", "softDelete"], Json, Ok),
    $crate::METHOD_SESSION_SHARE => share_session($crate::SessionShareParams) -> $crate::SessionShareResponse =>
        ["Share a session with the universe", "Moves an unshared root session to universe visibility, one way; its delegated children follow it. Refused on a bot's session, a delegated child, and a session already shared. Core applies it for any caller of the method; who may share is the caller's gate's decision."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ShareSession),
        http: (POST, "/api/v1/universes/{universe}/sessions/{sessionId}/share", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_EVENTS_READ => read_session_events($crate::SessionEventsReadParams) -> $crate::SessionEventsReadResponse =>
        ["Read the session event stream", "Returns chronological events. Forward (default) follows after and supports long-polling. Backward reads the latest window below before (or the head); pass nextCursor as before until complete. Follow live events after the initial backward headCursor. Windows may split runs/tool batches; keep historical reconstruction separate from live controls."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/sessions/{sessionId}/events", [("sessionId", "sessionId")], ["after", "before", "direction", "limit", "waitMs"], Json, Ok),
    $crate::METHOD_SESSION_CONTEXT_APPEND => append_context($crate::ContextAppendParams) -> $crate::ContextAppendResponse =>
        ["Append keyed session context", "Admits a batch of keyed context entries with per-entry results. The same content under a key is a no-op. Different content replaces the earlier entry, which leaves active context, and goes last under a new id; a catalog supersedes instead. Invalid input can fail one entry without discarding successful entries."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ControlSession),
        http: (POST, "/api/v1/universes/{universe}/sessions/{sessionId}/context/append", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_CONTEXT_REMOVE => remove_context($crate::ContextRemoveParams) -> $crate::ContextRemoveResponse =>
        ["Remove keyed session context", "Removes active entries by stable key with per-key results. Missing keys are idempotent no-ops; runtime-reserved run keys cannot be removed."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ControlSession),
        http: (POST, "/api/v1/universes/{universe}/sessions/{sessionId}/context/remove", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_CONTEXT_REPLACE => replace_context($crate::ContextReplaceParams) -> $crate::ContextReplaceResponse =>
        ["Replace session context entries", "Replaces active tool results and user messages in place by entry id, with per-entry results, e.g. to withdraw content the provider rejects. Entries keep their ids, positions, and kinds, so a tool result takes only text and its call stays answered. Refused while a run is active; ids no longer in context report absent."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ControlSession),
        http: (POST, "/api/v1/universes/{universe}/sessions/{sessionId}/context/replace", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_CONTEXT_COMPACT => compact_context($crate::ContextCompactParams) -> $crate::ContextCompactResponse =>
        ["Compact session context", "Performs one standalone compaction in any automatic mode, including Disabled, and waits for its outcome. Active work queues it until a safe turn boundary. A failed compaction leaves the context unchanged and fails the call with its reason. A wait that runs out fails with operationPending while it can still complete; session events report the outcome."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ControlSession),
        http: (POST, "/api/v1/universes/{universe}/sessions/{sessionId}/context/compact", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_RUNS_START => start_run($crate::RunStartParams) -> $crate::RunStartResponse =>
        ["Start an agent run", "Returns once the run is accepted — queued behind an active run, or running — not when it finishes. A compacting session holds the request; if the wait runs out the call fails with operationPending and the submission id, and the run still starts later. Retrying with that id cannot start a second run. Follow session events or reread the session."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ControlSession),
        http: (POST, "/api/v1/universes/{universe}/sessions/{sessionId}/runs", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_RUNS_LIST => list_runs($crate::RunListParams) -> $crate::RunListResponse =>
        ["List session runs", "Returns a newest-first keyset page of bounded run summaries, from current state for recent runs and from the session's log for older ones."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/sessions/{sessionId}/runs", [("sessionId", "sessionId")], ["cursor", "limit"], Json, Ok),
    $crate::METHOD_SESSION_RUNS_READ => read_run($crate::RunReadParams) -> $crate::RunReadResponse =>
        ["Read one session run", "Projects one run from its events. A run past the server's detail limit keeps its opening and its most recent turns and sets earlierEventsOmitted; session/events/read has the rest."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/sessions/{sessionId}/runs/{runId}", [("sessionId", "sessionId"), ("runId", "runId")], [], Json, Ok),
    $crate::METHOD_SESSION_RUNS_CANCEL => cancel_run($crate::RunCancelParams) -> $crate::RunCancelResponse =>
        ["Cancel a run", "Requests cancellation of the named queued or active run and returns its summary; observe session events for terminal completion. In-flight model and tool activity is aborted; no grace turn runs."], access: $crate::MethodAccess::Universe($crate::UniverseAction::StopSession),
        http: (POST, "/api/v1/universes/{universe}/sessions/{sessionId}/runs/{runId}/cancel", [("sessionId", "sessionId"), ("runId", "runId")], [], Json, Ok),
    $crate::METHOD_SESSION_RUNS_APPROVALS_DECIDE => decide_run_approvals($crate::RunApprovalsDecideParams) -> $crate::RunApprovalsDecideResponse =>
        ["Decide pending run approvals", "Approves or rejects pending MCP tool calls on the named active run. Valid decisions apply independently; the run resumes only after every pending approval has a decision. A decision whose wait runs out reports status pending and can still apply."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ControlSession),
        http: (POST, "/api/v1/universes/{universe}/sessions/{sessionId}/runs/{runId}/approvals/decide", [("sessionId", "sessionId"), ("runId", "runId")], [], Json, Ok),
    $crate::METHOD_SESSION_RUNS_STEER => steer_run($crate::RunSteerParams) -> $crate::RunSteerResponse =>
        ["Steer the active run", "Injects input into the named active run; the model sees it at the next turn boundary without interrupting the in-flight turn. A parked run holds it without waking. Rejected for queued, cancelling, or finished runs. Supply submissionId so a retry lands once; a wait that runs out fails with operationPending and the steering can still land."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ControlSession),
        http: (POST, "/api/v1/universes/{universe}/sessions/{sessionId}/runs/{runId}/steer", [("sessionId", "sessionId"), ("runId", "runId")], [], Json, Ok),
    $crate::METHOD_SESSION_SKILLS_LIST => list_skills($crate::SkillListParams) -> $crate::SkillListResponse =>
        ["List available session skills", "Returns separate VFS and environment catalogs with source, reference, availability, readable skill paths, and warnings. Refreshes only when open with no active or queued run, without waking environments. Absent catalogs are omitted."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/sessions/{sessionId}/skills", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_PROFILES_APPLY => apply_profile($crate::ProfileApplyParams) -> $crate::ProfileApplyResponse =>
        ["Apply a profile to a session", "Applies a named or inline profile's config, instructions, and environment setup to an existing session; mutating profile sections require it to be open and idle. Pass current revisions to guard concurrent changes."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureSession),
        http: (POST, "/api/v1/universes/{universe}/sessions/{sessionId}/profiles/apply", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_ENVIRONMENTS_ACTIVATE => activate_session_environment($crate::SessionEnvironmentActivateParams) -> $crate::SessionEnvironmentActivateResponse =>
        ["Activate a session environment", "Selects an attached, live universe environment for environment-targeted tools while the session is idle."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureSession),
        http: (PUT, "/api/v1/universes/{universe}/sessions/{sessionId}/active-environment", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_SESSION_ENVIRONMENTS_DEACTIVATE => deactivate_session_environment($crate::SessionEnvironmentDeactivateParams) -> $crate::SessionEnvironmentDeactivateResponse =>
        ["Deactivate the session environment", "Clears active environment selection without changing or closing the universe environment."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureSession),
        http: (DELETE, "/api/v1/universes/{universe}/sessions/{sessionId}/active-environment", [("sessionId", "sessionId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_CREDENTIALS_BIND => bind_environment_credential($crate::EnvironmentCredentialBindParams) -> $crate::EnvironmentCredentialBindResponse =>
        ["Bind a credential into an environment", "Maps an environment variable name to a retrievable credential grant or to a model API key provider's stored key. Commands in the environment receive the plain value, so a brokered grant and any other provider credential are refused. Requires configuring resources in the universe. The response exposes only the source handle, never secret material."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (PUT, "/api/v1/universes/{universe}/environments/{environmentId}/credentials/{envName}", [("environmentId", "environmentId"), ("envName", "envName")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_CREDENTIALS_LIST => list_environment_credentials($crate::EnvironmentCredentialListParams) -> $crate::EnvironmentCredentialListResponse =>
        ["List environment credential bindings", "Returns variable names and credential source handles for a universe environment; resolved secret values are never returned."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (GET, "/api/v1/universes/{universe}/environments/{environmentId}/credentials", [("environmentId", "environmentId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_CREDENTIALS_UNBIND => unbind_environment_credential($crate::EnvironmentCredentialUnbindParams) -> $crate::EnvironmentCredentialUnbindResponse =>
        ["Unbind an environment credential", "Removes one variable-to-credential mapping without deleting the underlying grant or provider credential."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (DELETE, "/api/v1/universes/{universe}/environments/{environmentId}/credentials/{envName}", [("environmentId", "environmentId"), ("envName", "envName")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_CREATE => create_environment($crate::EnvironmentCreateParams) -> $crate::EnvironmentCreateResponse =>
        ["Create an environment", "Creates a provisioned environment, attributed to the caller, from a template of a provider enabled for the universe. Idempotent per requestId: same inputs, same environment; others conflict. Provisioning until the target's first envd handshake, then open; idle overrides the template default."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/environments", [], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_READ => read_environment($crate::EnvironmentReadParams) -> $crate::EnvironmentReadResponse =>
        ["Read an environment", "Returns one environment: its source, lifecycle state, derived display status, state message, close reason, daemon build, and the latest provider observation."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/environments/{environmentId}", [("environmentId", "environmentId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_UPDATE => update_environment($crate::EnvironmentUpdateParams) -> $crate::EnvironmentUpdateResponse =>
        ["Update an environment", "Replaces the display name and caller metadata of an environment of any kind. Refused for a closed environment."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (PATCH, "/api/v1/universes/{universe}/environments/{environmentId}", [("environmentId", "environmentId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_LIST => list_environments($crate::EnvironmentListParams) -> $crate::EnvironmentListResponse =>
        ["List environments", "Lists the universe environments in id order, one page at a time, optionally filtered by provider, registration key, state, or metadata. Closed environments are omitted unless includeClosed is set or state is closed. Continue from nextAfter."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/environments", [], ["after", "includeClosed", "limit", "metadata", "providerId", "registrationKeyId", "state"], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_CLOSE => close_environment($crate::EnvironmentCloseParams) -> $crate::EnvironmentCloseResponse =>
        ["Close an environment", "Closes an environment; idempotent. A provisioned environment moves to closing while Lightspeed asks its provider to remove the target, retrying until it is gone, then closed. External and registered environments close at once, and a connected registered daemon is told its environment closed."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/environments/{environmentId}/close", [("environmentId", "environmentId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_EXTERNAL_CREATE => create_external_environment($crate::EnvironmentExternalCreateParams) -> $crate::EnvironmentExternalCreateResponse =>
        ["Register an external environment", "Creates an open environment backed by a Lightspeed-reachable envd endpoint, attributed to the caller. Idempotent per requestId. Reachability is checked on use."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/external-environments", [], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_REGISTERED_PUT => put_registered_environment($crate::EnvironmentRegisteredPutParams) -> $crate::EnvironmentRegisteredPutResponse =>
        ["Declare a registered environment", "Declares a fixed machine as an open environment with a chosen id before its daemon registers. The first daemon its persistent key admits under the given name is bound to it; until then it is away and tools wait. Repeating it changes nothing; display name and metadata converge. The key and name are fixed. Conflict: an id or name in use."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (PUT, "/api/v1/universes/{universe}/environments/{environmentId}/registration", [("environmentId", "environmentId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_REGISTERED_RELEASE => release_registered_environment($crate::EnvironmentRegisteredReleaseParams) -> $crate::EnvironmentRegisteredReleaseResponse =>
        ["Release a declared environment's daemon", "Forgets the daemon bound to a declared environment and ends its connection, so another daemon identity may bind under the same key and name. The environment keeps its id, its name, its credential bindings and every reference to it. No effect when no daemon is bound. Refused for an environment that was not declared."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/environments/{environmentId}/release", [("environmentId", "environmentId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_INGRESS_PUT => put_environment_ingress($crate::EnvironmentIngressPutParams) -> $crate::EnvironmentIngressPutResponse =>
        ["Configure environment public ingress", "Synchronously ensures or removes public ingress of an open provisioned environment through its provider; source.publicEndpoint carries the reported endpoint. Rejected when the provider offers no ingress; an internal error, safe to retry, when the provider is unreachable or times out. The provider owns hostname, guest port, routing, TLS, and health."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (PUT, "/api/v1/universes/{universe}/environments/{environmentId}/ingress", [("environmentId", "environmentId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_POWER_PUT => put_environment_power($crate::EnvironmentPowerPutParams) -> $crate::EnvironmentPowerPutResponse =>
        ["Set environment power", "Synchronously moves an open provisioned environment's target to running, paused, suspended, or stopped via its provider and records the observation; the provider may also change power on idle. Rejected without provider power control or when the template lacks the state; an internal error, safe to retry, when the provider is unreachable or times out."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (PUT, "/api/v1/universes/{universe}/environments/{environmentId}/power", [("environmentId", "environmentId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_PROVIDERS_LIST => list_available_environment_providers($crate::AvailableEnvironmentProviderListParams) -> $crate::AvailableEnvironmentProviderListResponse =>
        ["List available environment providers", "Lists the deployment providers this universe may create environments from: those available to every universe, and those this universe is bound to."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/environment-providers", [], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_PROVIDERS_READ => read_available_environment_provider($crate::AvailableEnvironmentProviderReadParams) -> $crate::AvailableEnvironmentProviderReadResponse =>
        ["Read an available environment provider", "Returns one provider this universe may create environments from; not found when it may not. Templates and physical resource, network, and ingress policy remain provider-owned."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/environment-providers/{providerId}", [("providerId", "providerId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_TEMPLATES_LIST => list_environment_templates($crate::EnvironmentTemplateListParams) -> $crate::EnvironmentTemplateListResponse =>
        ["List environment templates", "Reads templates live from the controllers of every provider available to the universe, or of one provider: compute, power states, public ingress, and default idle policy."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/environment-providers/{providerId}/templates", [("providerId", "providerId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_TEMPLATES_READ => read_environment_template($crate::EnvironmentTemplateReadParams) -> $crate::EnvironmentTemplateReadResponse =>
        ["Read an environment template", "Returns one template, read live from the controller of a provider available to the universe."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/environment-providers/{providerId}/templates/{templateId}", [("providerId", "providerId"), ("templateId", "templateId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_JOBS_CREATE => create_environment_jobs($crate::EnvironmentJobCreateParams) -> $crate::EnvironmentJobCreateResponse =>
        ["Create environment jobs", "Starts a dependency-aware job group on one environment instance, injecting its configured credentials at provider start. requestId is the retry identity; jobs are owned by the instance, not a session. A powered-down environment wakes on its data route; the call fails with environmentNotReady while it wakes or reconnects; retry with backoff."], access: $crate::MethodAccess::Universe($crate::UniverseAction::UseResource),
        http: (POST, "/api/v1/universes/{universe}/environments/{environmentId}/jobs", [("environmentId", "environmentId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_JOBS_READ => read_environment_jobs($crate::EnvironmentJobReadParams) -> $crate::EnvironmentJobReadResponse =>
        ["Read environment jobs", "Reads selected job handles with bounded output from a byte offset or the output tail; continue from each entry's outputNext and poll on the returned status."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/environment-jobs", [], ["after", "jobs", "maxBytes", "tailBytes"], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_JOBS_CANCEL => cancel_environment_jobs($crate::EnvironmentJobCancelParams) -> $crate::EnvironmentJobCancelResponse =>
        ["Cancel environment jobs", "Requests cancellation for selected jobs, optionally including dependents. Force is provider-specific escalation; inspect each per-job result."], access: $crate::MethodAccess::Universe($crate::UniverseAction::UseResource),
        http: (POST, "/api/v1/universes/{universe}/environment-jobs/cancel", [], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_CREATE => create_environment_registration_key($crate::EnvironmentRegistrationKeyCreateParams) -> $crate::EnvironmentRegistrationKeyCreateResponse =>
        ["Mint an environment registration key", "Creates a reusable universe-scoped key that lets outbound envd daemons register as environments. The plaintext secret is returned exactly once; only its hash is stored. Identity mode (immutable), active limit, disconnect grace, offline close-after, and expiry are the key's policy. Treat the secret like a cluster-join credential."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/environment-registration-keys", [], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_PUT => put_environment_registration_key($crate::EnvironmentRegistrationKeyPutParams) -> $crate::EnvironmentRegistrationKeyPutResponse =>
        ["Declare an environment registration key", "Makes the key of this id exist with this policy and answer to exactly the supplied secret. Repeating it changes nothing. A missing key is created; a different secret retires the previous one at once, as rotation does. Conflict: a changed identity mode, a revoked key, or a secret that is or was a key's."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (PUT, "/api/v1/universes/{universe}/environment-registration-keys/{registrationKeyId}", [("registrationKeyId", "registrationKeyId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_READ => read_environment_registration_key($crate::EnvironmentRegistrationKeyReadParams) -> $crate::EnvironmentRegistrationKeyReadResponse =>
        ["Read an environment registration key", "Returns the key's display prefix, policy, status, and derived environment counts; never the secret or its hash."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (GET, "/api/v1/universes/{universe}/environment-registration-keys/{registrationKeyId}", [("registrationKeyId", "registrationKeyId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_LIST => list_environment_registration_keys($crate::EnvironmentRegistrationKeyListParams) -> $crate::EnvironmentRegistrationKeyListResponse =>
        ["List environment registration keys", "Lists this universe's registration keys with policy, status, and derived counts. Each key is the group of the environments it admitted."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (GET, "/api/v1/universes/{universe}/environment-registration-keys", [], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_UPDATE => update_environment_registration_key($crate::EnvironmentRegistrationKeyUpdateParams) -> $crate::EnvironmentRegistrationKeyUpdateResponse =>
        ["Update an environment registration key", "Replaces a key's editable policy: display name, active limit, disconnect grace, offline close-after, and expiry; an omitted field is unset. The identity mode is immutable. The active limit and expiry apply to later admissions; a changed disconnect grace or offline close-after applies at the next sweep to environments whose daemon is away."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (PATCH, "/api/v1/universes/{universe}/environment-registration-keys/{registrationKeyId}", [("registrationKeyId", "registrationKeyId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_ROTATE => rotate_environment_registration_key($crate::EnvironmentRegistrationKeyRotateParams) -> $crate::EnvironmentRegistrationKeyRotateResponse =>
        ["Rotate an environment registration key", "Replaces the key's secret and display prefix and retires the old secret at once. The key keeps its id, group, policy, and the environments it admitted; daemons already registered are unaffected because they never present the key again. Returns the new secret once. Refused for a revoked key."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/environment-registration-keys/{registrationKeyId}/rotate", [("registrationKeyId", "registrationKeyId")], [], Json, Ok),
    $crate::METHOD_ENVIRONMENTS_REGISTRATION_KEYS_REVOKE => revoke_environment_registration_key($crate::EnvironmentRegistrationKeyRevokeParams) -> $crate::EnvironmentRegistrationKeyRevokeResponse =>
        ["Revoke an environment registration key", "Stops the key from admitting new daemon identities; already registered daemons keep reconnecting. With closeEnvironments, also closes every non-closed environment the key admitted. Idempotent."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/environment-registration-keys/{registrationKeyId}/revoke", [("registrationKeyId", "registrationKeyId")], [], Json, Ok),
    $crate::METHOD_TRANSCRIPTIONS_START => start_transcription($crate::TranscriptionStartParams) -> $crate::TranscriptionResponse =>
        ["Start transcription", "Admit or rejoin a requester-scoped audio transcription. The resolved model is immutable. No session or run is created."], access: $crate::MethodAccess::Universe($crate::UniverseAction::UseResource),
        http: (POST, "/api/v1/universes/{universe}/transcriptions", [], [], Json, Ok),
    $crate::METHOD_TRANSCRIPTIONS_READ => read_transcription($crate::TranscriptionReadParams) -> $crate::TranscriptionResponse =>
        ["Read transcription", "Read status and transcript text. Caller visibility is enforced by the external gate. Unsubmitted CAS results may expire."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/transcriptions/{transcriptionId}", [("transcriptionId", "transcriptionId")], [], Json, Ok),
    $crate::METHOD_TRANSCRIPTIONS_CANCEL => cancel_transcription($crate::TranscriptionCancelParams) -> $crate::TranscriptionResponse =>
        ["Cancel transcription", "Cancel unfinished transcription. Repeated cancellation is safe; completed results remain unchanged. Caller ownership is enforced by the external gate."], access: $crate::MethodAccess::Universe($crate::UniverseAction::UseResource),
        http: (POST, "/api/v1/universes/{universe}/transcriptions/{transcriptionId}/cancel", [("transcriptionId", "transcriptionId")], [], Json, Ok),
    $crate::METHOD_MODELS_DEFAULTS_READ => read_model_defaults($crate::ModelDefaultsReadParams) -> $crate::ModelDefaultsResponse =>
        ["Read universe model defaults", "Returns the revision and independent agentRun and speechToText selections. Revision zero means no update has been made. Does not contact model providers."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/model-defaults", [], [], Json, Ok),
    $crate::METHOD_MODELS_DEFAULTS_PUT => put_model_defaults($crate::ModelDefaultsPutParams) -> $crate::ModelDefaultsResponse =>
        ["Set a universe model default", "Sets or explicitly clears one purpose slot. With expectedRevision the write is refused if the defaults changed since. Existing sessions and admitted work keep their model. Validates the purpose and protocol without contacting provider discovery."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (PUT, "/api/v1/universes/{universe}/model-defaults/{slot}", [("slot", "slot")], [], Json, Ok),
    $crate::METHOD_MODELS_LIST => list_models($crate::ModelListParams) -> $crate::ModelListResponse =>
        ["Discover available models", "Queries supported providers directly, with a brief process-local burst cache, and returns best-effort selectable routes. One provider failure does not discard successful results from others."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/models", [], ["selectableOnly"], Json, Ok),
    $crate::METHOD_PROFILES_CREATE => create_profile($crate::ProfileCreateParams) -> $crate::ProfileCreateResponse =>
        ["Create an agent profile", "Creates a new universe-scoped reusable profile document; use profiles/put for create-or-replace revision semantics."], access: $crate::MethodAccess::Universe($crate::UniverseAction::CreateProfile),
        http: (POST, "/api/v1/universes/{universe}/profiles", [], [], Json, Ok),
    $crate::METHOD_PROFILES_READ => read_profile($crate::ProfileReadParams) -> $crate::ProfileReadResponse =>
        ["Read an agent profile", "Returns the complete profile document and current revision."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/profiles/{profileId}", [("profileId", "profileId")], [], Json, Ok),
    $crate::METHOD_PROFILES_LIST => list_profiles($crate::ProfileListParams) -> $crate::ProfileListResponse =>
        ["List agent profiles", "Returns lightweight summaries of universe-scoped reusable profiles."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/profiles", [], [], Json, Ok),
    $crate::METHOD_PROFILES_PUT => put_profile($crate::ProfilePutParams) -> $crate::ProfilePutResponse =>
        ["Create or replace an agent profile", "Stores the complete profile document. Use expectedRevision from profiles/read when replacing to prevent lost updates; absence writes unconditionally."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ManageProfile),
        http: (PUT, "/api/v1/universes/{universe}/profiles/{profileId}", [("profileId", "profile.profileId")], [], Json, Ok),
    $crate::METHOD_PROFILES_DELETE => delete_profile($crate::ProfileDeleteParams) -> $crate::ProfileDeleteResponse =>
        ["Delete an agent profile", "Deletes the catalog document; sessions previously created or configured from it retain their materialized state."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ManageProfile),
        http: (DELETE, "/api/v1/universes/{universe}/profiles/{profileId}", [("profileId", "profileId")], [], Json, Ok),
    $crate::METHOD_BLOBS_PUT => put_blobs($crate::BlobPutParams) -> $crate::BlobPutResponse =>
        ["Store content-addressed blobs", "Decodes and stores a batch of base64 payloads, returning immutable content references in request order. Re-uploading identical bytes is naturally deduplicated."], access: $crate::MethodAccess::Universe($crate::UniverseAction::UseResource),
        http: (POST, "/api/v1/universes/{universe}/blobs/batch", [], [], Json, Ok),
    $crate::METHOD_BLOBS_READ => read_blob($crate::BlobReadParams) -> Vec<u8> =>
        ["Read a content-addressed blob", "Returns the complete immutable blob of this universe as base64; large values count against gateway and MCP response limits."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/blobs/{blobRef}/content", [("blobRef", "blobRef")], [], BinaryDownload, Ok),
    $crate::METHOD_BLOBS_HAS => has_blobs($crate::BlobHasParams) -> $crate::BlobHasResponse =>
        ["Check blob availability", "Checks a batch of content references without returning blob bodies, preserving request order."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/blobs/presence", [], ["blobRefs"], Json, Ok),
    $crate::METHOD_VFS_SNAPSHOTS_COMMIT => commit_vfs_snapshot($crate::VfsSnapshotCommitParams) -> $crate::VfsSnapshotCommitResponse =>
        ["Commit a VFS snapshot", "Validates and stores an immutable filesystem manifest. Upload referenced file blobs first; the returned snapshot ref is content-addressed."], access: $crate::MethodAccess::Universe($crate::UniverseAction::UseResource),
        http: (POST, "/api/v1/universes/{universe}/vfs/snapshots", [], [], Json, Ok),
    $crate::METHOD_VFS_SNAPSHOTS_READ => read_vfs_snapshot($crate::VfsSnapshotReadParams) -> $crate::VfsSnapshotReadResponse =>
        ["Read a VFS snapshot", "Returns an immutable snapshot manifest and aggregate file/byte counts; file bodies remain separate blobs."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/vfs/snapshots/{snapshotRef}", [("snapshotRef", "snapshotRef")], [], Json, Ok),
    $crate::METHOD_VFS_WORKSPACES_CREATE => create_vfs_workspace($crate::VfsWorkspaceCreateParams) -> $crate::VfsWorkspaceCreateResponse =>
        ["Create a mutable VFS workspace", "Creates a universe workspace attributed to the caller at an optional seed snapshot; absence starts from a server-created empty snapshot."], access: $crate::MethodAccess::Universe($crate::UniverseAction::CreateWorkspace),
        http: (POST, "/api/v1/universes/{universe}/vfs/workspaces", [], [], Json, Ok),
    $crate::METHOD_VFS_WORKSPACES_READ => read_vfs_workspace($crate::VfsWorkspaceReadParams) -> $crate::VfsWorkspaceReadResponse =>
        ["Read a VFS workspace", "Returns workspace metadata, current head snapshot, and revision for safe updates."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/vfs/workspaces/{workspaceId}", [("workspaceId", "workspaceId")], [], Json, Ok),
    $crate::METHOD_VFS_WORKSPACES_LIST => list_vfs_workspaces($crate::VfsWorkspaceListParams) -> $crate::VfsWorkspaceListResponse =>
        ["List VFS workspaces", "Lists the mutable universe workspaces with head snapshots, sizes, and revisions."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/vfs/workspaces", [], [], Json, Ok),
    $crate::METHOD_VFS_WORKSPACES_UPDATE => update_vfs_workspace($crate::VfsWorkspaceUpdateParams) -> $crate::VfsWorkspaceUpdateResponse =>
        ["Update a VFS workspace", "Moves the workspace head to an existing snapshot, which requires use of the workspace, and updates its display name, which requires configuring it. Pass expectedRevision from a read to prevent lost updates."], access: $crate::MethodAccess::Universe($crate::UniverseAction::UseResource),
        http: (PATCH, "/api/v1/universes/{universe}/vfs/workspaces/{workspaceId}", [("workspaceId", "workspaceId")], [], Json, Ok),
    $crate::METHOD_VFS_WORKSPACES_DELETE => delete_vfs_workspace($crate::VfsWorkspaceDeleteParams) -> $crate::VfsWorkspaceDeleteResponse =>
        ["Delete a VFS workspace", "Deletes the mutable workspace record; immutable snapshots and blobs remain content-addressed resources."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (DELETE, "/api/v1/universes/{universe}/vfs/workspaces/{workspaceId}", [("workspaceId", "workspaceId")], [], Json, Ok),
    $crate::METHOD_MCP_SERVERS_PUT => put_mcp_server($crate::McpServerPutParams) -> $crate::McpServerPutResponse =>
        ["Create or replace an MCP server record", "Stores the complete catalog document with its optional credential grant. A new server is attributed to the caller. Use expectedRevision when replacing; token material is never accepted or returned, but serverUrl is stored and shown as written, so keep secrets out of it."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (PUT, "/api/v1/universes/{universe}/mcp/servers/{serverId}", [("serverId", "server.serverId")], [], Json, Ok),
    $crate::METHOD_MCP_SERVERS_AUTH_DISCOVER => discover_mcp_server_auth($crate::McpServerAuthDiscoverParams) -> $crate::McpServerAuthDiscoverResponse =>
        ["Discover MCP server authentication", "Looks for standards-based OAuth protected-resource metadata without creating a server, OAuth client, flow, or grant. An absent OAuth result is inconclusive and callers must allow manual auth selection."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/mcp/auth/discover", [], [], Json, Ok),
    $crate::METHOD_MCP_SERVERS_TOOLS_DISCOVER => discover_mcp_server_tools($crate::McpServerToolsDiscoverParams) -> $crate::McpServerToolsDiscoverResponse =>
        ["Discover MCP server tools", "Connects directly to the configured MCP server with its current universe credential and returns one bounded live tools/list result. The inventory is never persisted or cached and no tool is invoked."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/mcp/servers/{serverId}/tools/discover", [("serverId", "serverId")], [], Json, Ok),
    $crate::METHOD_MCP_SERVERS_READ => read_mcp_server($crate::McpServerReadParams) -> $crate::McpServerReadResponse =>
        ["Read an MCP server record", "Returns one catalog document with defaults, auth policy, non-secret grant binding, status, and revision; no credential value is exposed."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/mcp/servers/{serverId}", [("serverId", "serverId")], [], Json, Ok),
    $crate::METHOD_MCP_SERVERS_LIST => list_mcp_servers($crate::McpServerListParams) -> $crate::McpServerListResponse =>
        ["List MCP server records", "Lists the universe catalog entries, optionally filtered by lifecycle/configuration status."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/mcp/servers", [], ["status"], Json, Ok),
    $crate::METHOD_MCP_SERVERS_DELETE => delete_mcp_server($crate::McpServerDeleteParams) -> $crate::McpServerDeleteResponse =>
        ["Delete an MCP server record", "Deletes the catalog document. Existing session configs that reference it are not silently rewritten and may need explicit reconfiguration."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (DELETE, "/api/v1/universes/{universe}/mcp/servers/{serverId}", [("serverId", "serverId")], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_GRANTS_IMPORT => import_credential_grant($crate::CredentialGrantImportParams) -> $crate::CredentialGrantImportResponse =>
        ["Import a token credential", "Accepts a plaintext token, encrypts it immediately, and returns only grant metadata/token-presence flags. Brokered is the default; exposure is immutable, and only a retrievable credential can be leased to a service or bound into an environment."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/credentials/grants/actions/import", [], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_GRANTS_LEASE => lease_credential_grant($crate::CredentialGrantLeaseParams) -> $crate::CredentialGrantLeaseResponse =>
        ["Lease a retrievable credential grant", "Service callers only. Resolves the current access token through the broker, records the lease, and returns it once. Cache only in memory until expiry minus margin (or at most five minutes without expiry), re-lease after target 401/403, and never persist or place the token in workflow payloads."], access: $crate::MethodAccess::Service,
        http: (POST, "/api/v1/universes/{universe}/credentials/grants/{grantId}/lease", [("grantId", "grantId")], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_GRANTS_READ => read_credential_grant($crate::CredentialGrantReadParams) -> $crate::CredentialGrantReadResponse =>
        ["Read credential grant metadata", "Returns creator attribution, provider binding, scopes, audience, expiry, status, and token-presence flags; access and refresh token values are never returned."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/credentials/grants/{grantId}", [("grantId", "grantId")], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_GRANTS_LIST => list_credential_grants($crate::CredentialGrantListParams) -> $crate::CredentialGrantListResponse =>
        ["List credential grants", "Lists non-secret grant metadata for the universe, optionally filtered by status."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/credentials/grants", [], ["status"], Json, Ok),
    $crate::METHOD_CREDENTIALS_GRANTS_PUT => put_credential_grant($crate::CredentialGrantPutParams) -> $crate::CredentialGrantPutResponse =>
        ["Declare a token credential", "Makes the token credential of this id exist with these settings and hold exactly the supplied token, encrypted on receipt. Repeating it changes nothing. A missing credential is created; a different token replaces the stored one for everything referencing the id. Conflict: another kind of grant, a revoked one, or changed exposure."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (PUT, "/api/v1/universes/{universe}/credentials/grants/{grantId}", [("grantId", "grantId")], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_GRANTS_REVOKE => revoke_credential_grant($crate::CredentialGrantRevokeParams) -> $crate::CredentialGrantRevokeResponse =>
        ["Revoke a credential grant", "Marks the grant unusable by token consumers while retaining non-secret audit metadata."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/credentials/grants/{grantId}/revoke", [("grantId", "grantId")], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_CLIENTS_CREATE => create_credential_client($crate::CredentialClientCreateParams) -> $crate::CredentialClientCreateResponse =>
        ["Register an OAuth client", "Stores provider endpoints and client identity; an optional plaintext client secret is encrypted and represented thereafter only by hasClientSecret."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/credentials/clients", [], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_CLIENTS_READ => read_credential_client($crate::CredentialClientReadParams) -> $crate::CredentialClientReadResponse =>
        ["Read OAuth client metadata", "Returns endpoints, public client identity, defaults, and secret-presence state; the client secret is never returned."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/credentials/clients/{clientId}", [("clientId", "clientId")], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_CLIENTS_LIST => list_credential_clients($crate::CredentialClientListParams) -> $crate::CredentialClientListResponse =>
        ["List OAuth clients", "Lists non-secret OAuth client registrations in the universe."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/credentials/clients", [], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_CLIENTS_DELETE => delete_credential_client($crate::CredentialClientDeleteParams) -> $crate::CredentialClientDeleteResponse =>
        ["Delete an OAuth client", "Deletes the client registration and its stored client secret; grants already created from it remain separate records."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (DELETE, "/api/v1/universes/{universe}/credentials/clients/{clientId}", [("clientId", "clientId")], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_FLOWS_START => start_credential_flow($crate::CredentialFlowStartParams) -> $crate::CredentialFlowStartResponse =>
        ["Start an OAuth authorization flow", "Creates a short-lived PKCE flow carrying the immutable grant exposure choice and returns a browser authorization URL containing one-time state. Treat the URL as sensitive and poll credentials/flows/read for completion."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/credentials/flows", [], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_FLOWS_READ => read_credential_flow_status($crate::CredentialFlowStatusParams) -> $crate::CredentialFlowStatusResponse =>
        ["Read OAuth flow status", "Polls a flow's pending/completed/failed/expired state and returns the resulting grant id when authorization succeeds; no token value is exposed."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (GET, "/api/v1/universes/{universe}/credentials/flows/{flowId}", [("flowId", "flowId")], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_PROVIDERS_CREATE => create_credential_provider($crate::CredentialProviderCreateParams) -> $crate::CredentialProviderCreateResponse =>
        ["Register a credential provider", "Creates a model or GitHub credential source. Plaintext API keys/private keys are encrypted on receipt and later represented only by configuration plus hasCredential."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/credentials/providers", [], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_PROVIDERS_PUT => put_credential_provider($crate::CredentialProviderPutParams) -> $crate::CredentialProviderPutResponse =>
        ["Declare a credential provider", "Makes the provider of this id exist with this configuration and credential. Repeating it changes nothing. A missing provider is created; one that differs is rewritten in place after the new settings validate, atomically, and its old credential deleted. Sessions and environment variables referencing the id keep working."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (PUT, "/api/v1/universes/{universe}/credentials/providers/{providerId}", [("providerId", "providerId")], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_PROVIDERS_READ => read_credential_provider($crate::CredentialProviderReadParams) -> $crate::CredentialProviderReadResponse =>
        ["Read credential provider metadata", "Returns provider kind, non-secret configuration, credential-presence state, and status; stored credentials are never returned."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/credentials/providers/{providerId}", [("providerId", "providerId")], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_PROVIDERS_LIST => list_credential_providers($crate::CredentialProviderListParams) -> $crate::CredentialProviderListResponse =>
        ["List credential providers", "Lists non-secret model/GitHub provider registrations for the universe."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/credentials/providers", [], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_PROVIDERS_DELETE => delete_credential_provider($crate::CredentialProviderDeleteParams) -> $crate::CredentialProviderDeleteResponse =>
        ["Delete a credential provider", "Deletes the provider registration and its directly stored credential; separately stored grants remain independent records."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (DELETE, "/api/v1/universes/{universe}/credentials/providers/{providerId}", [("providerId", "providerId")], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_GITHUB_INSTALLATIONS_LIST => list_github_installations($crate::CredentialGitHubInstallationListParams) -> $crate::CredentialGitHubInstallationListResponse =>
        ["List GitHub App installations", "Uses the registered GitHub App provider credential to query accessible installations and returns account/permission metadata without tokens."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (GET, "/api/v1/universes/{universe}/credentials/providers/{providerId}/github/installations", [("providerId", "providerId")], [], Json, Ok),
    $crate::METHOD_CREDENTIALS_GITHUB_INSTALLATIONS_GRANT => grant_github_installation($crate::CredentialGitHubInstallationGrantParams) -> $crate::CredentialGitHubInstallationGrantResponse =>
        ["Grant access to a GitHub App installation", "Creates or refreshes a universe credential grant for one accessible installation. This method never returns a token: the broker mints short-lived installation tokens when the grant is used. Brokered is the default; only a grant created retrievable can be bound into an environment or leased to a service."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/credentials/providers/{providerId}/github/installations/{installationId}/grant", [("providerId", "providerId"), ("installationId", "installationId")], [], Json, Ok),
    $crate::METHOD_BOTS_CREATE => create_bot($crate::BotCreateParams) -> $crate::BotCreateResponse =>
        ["Create a bot", "Creates the bot record, optionally with its triggers, and starts its controller. Fails if the bot id exists; a trigger failure rolls the bot back. Returns the created triggers' ingest paths and pairing codes by trigger id."], access: $crate::MethodAccess::Universe($crate::UniverseAction::CreateBot),
        http: (POST, "/api/v1/universes/{universe}/bots", [], [], Json, Ok),
    $crate::METHOD_BOTS_PUT => put_bot($crate::BotPutParams) -> $crate::BotPutResponse =>
        ["Create or replace a bot document", "Replaces the mutable configuration whole and signals the controller, which applies it at its next idle boundary. Pass expectedRevision when replacing; a closed bot accepts label-only edits."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ManageBot),
        http: (PUT, "/api/v1/universes/{universe}/bots/{botId}", [("botId", "bot.botId")], [], Json, Ok),
    $crate::METHOD_BOTS_READ => read_bot($crate::BotReadParams) -> $crate::BotReadResponse =>
        ["Read a bot", "Returns the bot record, its current revision, and lifecycle columns."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/bots/{botId}", [("botId", "botId")], [], Json, Ok),
    $crate::METHOD_BOTS_LIST => list_bots($crate::BotListParams) -> $crate::BotListResponse =>
        ["List bots", "Returns the roster: every bot with its trigger count, pending event count, and latest event, optionally narrowed by createdBy."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/bots", [], ["createdBy"], Json, Ok),
    $crate::METHOD_BOTS_CLOSE => close_bot($crate::BotCloseParams) -> $crate::BotCloseResponse =>
        ["Close a bot", "Terminal and idempotent: disables every trigger, drops schedules, and tells the controller to archive pending events and force-close its sessions. Returns once signalled; follow bots/state/read for closing to closed."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ManageBot),
        http: (POST, "/api/v1/universes/{universe}/bots/{botId}/close", [("botId", "botId")], [], Json, Ok),
    $crate::METHOD_BOTS_DELETE => delete_bot($crate::BotDeleteParams) -> $crate::BotDeleteResponse =>
        ["Delete a bot", "Closes the bot if needed, waits for its controller to complete, retains its session history, and removes the record so the bot id is free again."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ManageBot),
        http: (DELETE, "/api/v1/universes/{universe}/bots/{botId}", [("botId", "botId")], [], Json, Ok),
    $crate::METHOD_BOTS_STATE_READ => read_bot_state($crate::BotStateReadParams) -> $crate::BotStateReadResponse =>
        ["Read bot controller state", "Queries the controller workflow for its live snapshot (sessions, buffers, active and recent deliveries, budget) and lists sub-agent descendants. The controller is absent until the bot's first event."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/bots/{botId}/state", [("botId", "botId")], [], Json, Ok),
    $crate::METHOD_BOTS_SESSIONS_ROTATE => rotate_bot_session($crate::BotSessionRotateParams) -> $crate::BotSessionRotateResponse =>
        ["Rotate a bot session", "Asks the controller to close one of the bot's sessions at its next idle boundary and continue on a fresh generation; queued deliveries follow."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ManageBot),
        http: (POST, "/api/v1/universes/{universe}/bots/{botId}/sessions/rotate", [("botId", "botId")], [], Json, Ok),
    $crate::METHOD_BOTS_TRIGGERS_PUT => put_bot_trigger($crate::BotTriggerPutParams) -> $crate::BotTriggerPutResponse =>
        ["Create or replace a trigger", "Validates the trigger document (CEL parses, grants exist, one inbox per bot, chat routes per conversation), reconciles its Temporal Schedule, and stores it. A poll spec edit resets the cursor; a webhook keeps its URL token. Returns the trigger's ingest path or pairing code beside the view."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ManageBot),
        http: (PUT, "/api/v1/universes/{universe}/bots/{botId}/triggers/{triggerId}", [("botId", "botId"), ("triggerId", "trigger.triggerId")], [], Json, Ok),
    $crate::METHOD_BOTS_TRIGGERS_READ => read_bot_trigger($crate::BotTriggerReadParams) -> $crate::BotTriggerReadResponse =>
        ["Read a trigger", "Returns one trigger with its incidents and cursor. The ingest path and pairing code are never part of it; bots/triggers/secrets/read returns them."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/bots/{botId}/triggers/{triggerId}", [("botId", "botId"), ("triggerId", "triggerId")], [], Json, Ok),
    $crate::METHOD_BOTS_TRIGGERS_LIST => list_bot_triggers($crate::BotTriggerListParams) -> $crate::BotTriggerListResponse =>
        ["List a bot's triggers", "Returns every trigger of the bot ordered by id, without ingest paths or pairing codes."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/bots/{botId}/triggers", [("botId", "botId")], [], Json, Ok),
    $crate::METHOD_BOTS_TRIGGERS_SECRETS_READ => read_bot_trigger_secrets($crate::BotTriggerSecretsReadParams) -> $crate::BotTriggerSecretsReadResponse =>
        ["Read a trigger's secrets", "Returns a webhook trigger's ingest path, whose token lets the holder post events to the bot, or a chat trigger's pairing code, which connects a conversation to it. Both are absent for other triggers. Needs bot management, unlike reading the trigger."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ManageBot),
        http: (GET, "/api/v1/universes/{universe}/bots/{botId}/triggers/{triggerId}/secrets", [("botId", "botId"), ("triggerId", "triggerId")], [], Json, Ok),
    $crate::METHOD_BOTS_TRIGGERS_DELETE => delete_bot_trigger($crate::BotTriggerDeleteParams) -> $crate::BotTriggerDeleteResponse =>
        ["Delete a trigger", "Drops the trigger's Temporal Schedule and pairings, then the record; stored events keep their history."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ManageBot),
        http: (DELETE, "/api/v1/universes/{universe}/bots/{botId}/triggers/{triggerId}", [("botId", "botId"), ("triggerId", "triggerId")], [], Json, Ok),
    $crate::METHOD_BOTS_EVENTS_ADMIT => admit_bot_event($crate::BotEventAdmitParams) -> $crate::BotEventAdmitResponse =>
        ["Admit an event manually", "Stores an operator-authored event for the bot's main session and wakes the controller. eventId is the dedupe identity; a duplicate returns the stored row."], access: $crate::MethodAccess::Universe($crate::UniverseAction::InvokeBot),
        http: (POST, "/api/v1/universes/{universe}/bots/{botId}/events", [("botId", "botId")], [], Json, Ok),
    $crate::METHOD_BOTS_EVENTS_REPLAY => replay_bot_event($crate::BotEventReplayParams) -> $crate::BotEventReplayResponse =>
        ["Replay a stored event", "Re-admits the stored envelope as a fresh event with the original routing; the replay never coalesces."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ManageBot),
        http: (POST, "/api/v1/universes/{universe}/bots/{botId}/events/{seq}/replay", [("botId", "botId"), ("seq", "seq")], [], Json, Ok),
    $crate::METHOD_BOTS_EVENTS_LIST => list_bot_events($crate::BotEventListParams) -> $crate::BotEventListResponse =>
        ["List a bot's events", "Cursor-paginated event log, newest first, with outcomes; payload documents stay in the CAS."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/bots/{botId}/events", [("botId", "botId")], ["cursor", "limit"], Json, Ok),
    $crate::METHOD_BOTS_EVENTS_READ => read_bot_event($crate::BotEventReadParams) -> $crate::BotEventReadResponse =>
        ["Read an event by number", "Returns the event row and its full stored envelope document."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/bots/{botId}/events/{seq}", [("botId", "botId"), ("seq", "seq")], [], Json, Ok),
    $crate::METHOD_BOTS_FILTERS_TEST => test_bot_filter($crate::BotFilterTestParams) -> $crate::BotFilterTestResponse =>
        ["Test a CEL filter", "Evaluates a filter against one payload or a sample of recent stored events, reporting matches and evaluation errors without changing anything."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ManageBot),
        http: (POST, "/api/v1/universes/{universe}/bots/{botId}/filters/test", [("botId", "botId")], [], Json, Ok),
    $crate::METHOD_CHANNELS_ACCOUNTS_CREATE => create_channel_account($crate::ChannelAccountCreateParams) -> $crate::ChannelAccountCreateResponse =>
        ["Create a channel account", "Registers a provider account (Telegram, WhatsApp) for this universe. The credential is a retrievable grant reference; no token is accepted here."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (POST, "/api/v1/universes/{universe}/channels/accounts", [], [], Json, Ok),
    $crate::METHOD_CHANNELS_ACCOUNTS_PUT => put_channel_account($crate::ChannelAccountPutParams) -> $crate::ChannelAccountPutResponse =>
        ["Create or replace a channel account", "Replaces the account document whole; pass expectedRevision when replacing. The connector host picks the change up on its next discovery pass."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (PUT, "/api/v1/universes/{universe}/channels/accounts/{accountId}", [("accountId", "account.accountId")], [], Json, Ok),
    $crate::METHOD_CHANNELS_ACCOUNTS_READ => read_channel_account($crate::ChannelAccountReadParams) -> $crate::ChannelAccountReadResponse =>
        ["Read a channel account", "Returns the account document and revision."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/channels/accounts/{accountId}", [("accountId", "accountId")], [], Json, Ok),
    $crate::METHOD_CHANNELS_ACCOUNTS_LIST => list_channel_accounts($crate::ChannelAccountListParams) -> $crate::ChannelAccountListResponse =>
        ["List channel accounts", "Lists this universe's provider accounts, optionally by provider."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/channels/accounts", [], ["provider"], Json, Ok),
    $crate::METHOD_CHANNELS_ACCOUNTS_DELETE => delete_channel_account($crate::ChannelAccountDeleteParams) -> $crate::ChannelAccountDeleteResponse =>
        ["Delete a channel account", "Removes the account and its pairings; chat triggers that reference it stop serving conversations."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (DELETE, "/api/v1/universes/{universe}/channels/accounts/{accountId}", [("accountId", "accountId")], [], Json, Ok),
    $crate::METHOD_CHANNELS_INBOUND_ADMIT => admit_channel_inbound($crate::ChannelInboundAdmitParams) -> $crate::ChannelInboundAdmitResponse =>
        ["Admit a provider message", "Service callers only. Resolves the chat trigger for the conversation, applies pairing, and signals the conversation workflow. Returns the decision so the connector can send pairing prompts itself; acknowledge the provider only after this returns."], access: $crate::MethodAccess::Service,
        http: (POST, "/api/v1/universes/{universe}/channels/accounts/{accountId}/inbound", [("accountId", "accountId")], [], Json, Ok),
    $crate::METHOD_CHANNELS_PAIRINGS_LIST => list_channel_pairings($crate::ChannelPairingListParams) -> $crate::ChannelPairingListResponse =>
        ["List chat pairings", "Lists conversations paired to chat triggers, optionally by account or bot."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/channels/pairings", [], ["accountId", "botId"], Json, Ok),
    $crate::METHOD_CHANNELS_PAIRINGS_DELETE => delete_channel_pairing($crate::ChannelPairingDeleteParams) -> $crate::ChannelPairingDeleteResponse =>
        ["Unpair a conversation", "Removes one pairing; the conversation must present the pairing code again to reconnect."], access: $crate::MethodAccess::Universe($crate::UniverseAction::ConfigureResource),
        http: (DELETE, "/api/v1/universes/{universe}/channels/accounts/{accountId}/pairings/{chatId}", [("accountId", "accountId"), ("chatId", "chatId")], [], Json, Ok),
    $crate::METHOD_CHANNELS_CONVERSATIONS_READ => read_channel_conversation($crate::ChannelConversationReadParams) -> $crate::ChannelConversationReadResponse =>
        ["Read a conversation snapshot", "Queries the conversation workflow's live state for one chat, for debugging; absent when no workflow exists yet."], access: $crate::MethodAccess::Universe($crate::UniverseAction::Read),
        http: (GET, "/api/v1/universes/{universe}/channels/accounts/{accountId}/conversations/{chatId}", [("accountId", "accountId"), ("chatId", "chatId")], ["threadId"], Json, Ok),
        }
    };
}

for_each_universe_operation!(api_methods);
