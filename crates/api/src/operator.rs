//! Operator-scoped (deployment-level) API contract.
//!
//! Operator methods address the deployment itself — the set of universes —
//! rather than acting inside one universe, so they form a second scope class
//! with its own service trait and dispatcher. They share the JSON-RPC
//! envelope, error model, and `/rpc` endpoint with the universe-scoped API;
//! the `operator/` method-name prefix is what routes a request here, and the
//! gateway enforces the authorization boundary before dispatch (trusted-header
//! and single modes only — never api-key callers).

use super::*;

/// Method-name prefix that marks the operator scope. Dispatch keys off this
/// mechanically, so the scope of every method is visible in its name.
pub const OPERATOR_METHOD_PREFIX: &str = "operator/";

pub const METHOD_OPERATOR_UNIVERSES_CREATE: &str = "operator/universes/create";
pub const METHOD_OPERATOR_UNIVERSES_LIST: &str = "operator/universes/list";
pub const METHOD_OPERATOR_UNIVERSES_READ: &str = "operator/universes/read";
pub const METHOD_OPERATOR_UNIVERSES_DELETE: &str = "operator/universes/delete";
pub const METHOD_OPERATOR_OUTBOX_READ: &str = "operator/outbox/read";

pub fn is_operator_method(method: &str) -> bool {
    method.starts_with(OPERATOR_METHOD_PREFIX)
}

/// Per-universe stats view. Counts are cheap aggregates computed at read
/// time, not maintained counters — approximate under concurrent writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorUniverseView {
    pub universe_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub created_at_ms: u64,
    /// Most recent session activity (`max(sessions.updated_at_ms)`); absent
    /// when the universe has no sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at_ms: Option<u64>,
    pub sessions: u64,
    pub workspaces: u64,
    pub profiles: u64,
    pub blob_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorUniverseCreateParams {
    pub universe_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorUniverseCreateResponse {
    pub universe: OperatorUniverseView,
    /// False when the universe already existed (create is idempotent).
    pub created: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorUniverseListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorUniverseListResponse {
    #[serde(default)]
    pub universes: Vec<OperatorUniverseView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorUniverseReadParams {
    pub universe_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorUniverseReadResponse {
    pub universe: OperatorUniverseView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorUniverseDeleteParams {
    pub universe_id: String,
}

/// Purge report. The purge is idempotent: rerunning after a partial failure
/// resumes where it stopped, and the universe row is deleted last so a
/// half-purged universe is still visible to `operator/universes/read`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorUniverseDeleteResponse {
    pub universe_id: String,
    /// Live session workflows terminated during the purge.
    pub workflows_terminated: u64,
    /// External object-store blobs deleted during the purge.
    pub blob_objects_deleted: u64,
}

/// Multiplexed outbox read: one long-poll serves every universe of the
/// deployment, replacing one `outbox/read` tailer per universe. `seq` is a
/// deployment-global cursor (the outbox sequence is one identity column
/// across universes), so a single `after` resumes the whole stream.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorOutboxReadParams {
    /// Return pending entries with `seq` greater than this cursor. Restart
    /// from 0 to re-read undelivered entries after a consumer restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Long-poll wait in milliseconds when no entries are pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u32>,
}

/// One pending outbox entry with its owning universe. Acknowledge through
/// the per-universe `outbox/ack` (with the entry's universe header); the
/// global cursor only drives reading.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorOutboundMessageView {
    pub universe_id: String,
    #[serde(flatten)]
    pub message: OutboundMessageView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperatorOutboxReadResponse {
    #[serde(default)]
    pub entries: Vec<OperatorOutboundMessageView>,
    /// Cursor to pass as `after` on the next read.
    pub next_after: u64,
}

#[async_trait]
pub trait OperatorApiService: Send + Sync {
    async fn create_universe(
        &self,
        params: OperatorUniverseCreateParams,
    ) -> Result<AgentApiOutcome<OperatorUniverseCreateResponse>, AgentApiError>;

    async fn list_universes(
        &self,
        params: OperatorUniverseListParams,
    ) -> Result<AgentApiOutcome<OperatorUniverseListResponse>, AgentApiError>;

    async fn read_universe(
        &self,
        params: OperatorUniverseReadParams,
    ) -> Result<AgentApiOutcome<OperatorUniverseReadResponse>, AgentApiError>;

    async fn delete_universe(
        &self,
        params: OperatorUniverseDeleteParams,
    ) -> Result<AgentApiOutcome<OperatorUniverseDeleteResponse>, AgentApiError>;

    async fn read_outbox(
        &self,
        params: OperatorOutboxReadParams,
    ) -> Result<AgentApiOutcome<OperatorOutboxReadResponse>, AgentApiError>;
}

macro_rules! operator_api_methods {
    ($($method_const:ident => $service_fn:ident($params:ty) -> $response:ty =>
        [$summary:expr, $description:expr]),+ $(,)?) => {
        pub async fn dispatch_operator_json_rpc(
            service: &dyn OperatorApiService,
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

        /// One entry per operator JSON-RPC method, in dispatch order.
        /// Generated by the same macro invocation as the dispatcher, so the
        /// manifest cannot drift from it.
        pub fn operator_method_manifest() -> Vec<MethodSpec> {
            vec![
                $(
                    MethodSpec {
                        method: $method_const,
                        scope: MethodScope::Operator,
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

operator_api_methods! {
    METHOD_OPERATOR_UNIVERSES_CREATE => create_universe(OperatorUniverseCreateParams) -> OperatorUniverseCreateResponse =>
        ["Create a universe", "Creates the deployment tenant boundary for an explicit UUID. The operation is idempotent and reports whether a new universe was created."],
    METHOD_OPERATOR_UNIVERSES_LIST => list_universes(OperatorUniverseListParams) -> OperatorUniverseListResponse =>
        ["List universes", "Returns deployment-wide universe summaries with approximate live aggregate counts and last session activity."],
    METHOD_OPERATOR_UNIVERSES_READ => read_universe(OperatorUniverseReadParams) -> OperatorUniverseReadResponse =>
        ["Read a universe", "Returns one deployment tenant summary with aggregate session, workspace, profile, and blob usage."],
    METHOD_OPERATOR_UNIVERSES_DELETE => delete_universe(OperatorUniverseDeleteParams) -> OperatorUniverseDeleteResponse =>
        ["Purge a universe", "Permanently terminates live session workflows, deletes external blob objects, and cascades universe data. The purge is resumable/idempotent after partial failure."],
    METHOD_OPERATOR_OUTBOX_READ => read_outbox(OperatorOutboxReadParams) -> OperatorOutboxReadResponse =>
        ["Read the deployment outbox", "Cursor-reads or long-polls pending messages across all universes. Entries identify their universe; acknowledge each through universe-scoped outbox/ack."],
}
