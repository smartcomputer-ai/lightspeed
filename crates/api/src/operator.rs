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
}

macro_rules! operator_api_methods {
    ($($method_const:ident => $service_fn:ident($params:ty) -> $response:ty),+ $(,)?) => {
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
    METHOD_OPERATOR_UNIVERSES_CREATE => create_universe(OperatorUniverseCreateParams) -> OperatorUniverseCreateResponse,
    METHOD_OPERATOR_UNIVERSES_LIST => list_universes(OperatorUniverseListParams) -> OperatorUniverseListResponse,
    METHOD_OPERATOR_UNIVERSES_READ => read_universe(OperatorUniverseReadParams) -> OperatorUniverseReadResponse,
    METHOD_OPERATOR_UNIVERSES_DELETE => delete_universe(OperatorUniverseDeleteParams) -> OperatorUniverseDeleteResponse,
}
