//! Deployment-scoped (deployment-level) API contract.
//!
//! Deployment methods address the deployment itself — the set of universes —
//! rather than acting inside one universe, so they form a second scope class
//! with its own service trait and dispatcher. They share the JSON-RPC
//! envelope, error model, and `/rpc` endpoint with the universe-scoped API;
//! the `deployment/` method-name prefix is what routes a request here, and the
//! gateway enforces the authorization boundary before dispatch (trusted-header
//! and single modes only — never api-key callers).

use super::*;

/// Method-name prefix that marks the deployment scope. Dispatch keys off this
/// mechanically, so the scope of every method is visible in its name.
pub const DEPLOYMENT_METHOD_PREFIX: &str = "deployment/";

pub const METHOD_DEPLOYMENT_UNIVERSES_CREATE: &str = "deployment/universes/create";
pub const METHOD_DEPLOYMENT_UNIVERSES_LIST: &str = "deployment/universes/list";
pub const METHOD_DEPLOYMENT_UNIVERSES_READ: &str = "deployment/universes/read";
pub const METHOD_DEPLOYMENT_UNIVERSES_DELETE: &str = "deployment/universes/delete";
pub const METHOD_DEPLOYMENT_API_KEYS_CREATE: &str = "deployment/api-keys/create";
pub const METHOD_DEPLOYMENT_API_KEYS_LIST: &str = "deployment/api-keys/list";
pub const METHOD_DEPLOYMENT_API_KEYS_REVOKE: &str = "deployment/api-keys/revoke";
pub const METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_PUT: &str =
    "deployment/environment-providers/put";
pub const METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_LIST: &str =
    "deployment/environment-providers/list";
pub const METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_READ: &str =
    "deployment/environment-providers/read";
pub const METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_DELETE: &str =
    "deployment/environment-providers/delete";
pub const METHOD_DEPLOYMENT_PROVIDER_BINDINGS_PUT: &str =
    "deployment/environment-providers/bindings/put";
pub const METHOD_DEPLOYMENT_PROVIDER_BINDINGS_DELETE: &str =
    "deployment/environment-providers/bindings/delete";
pub const METHOD_DEPLOYMENT_ENVIRONMENTS_ADOPT: &str = "deployment/environments/adopt";
pub const METHOD_DEPLOYMENT_CHANNELS_ACCOUNTS_LIST: &str = "deployment/channels/accounts/list";

pub fn is_deployment_method(method: &str) -> bool {
    method.starts_with(DEPLOYMENT_METHOD_PREFIX)
}

/// Per-universe stats view. Counts are cheap aggregates computed at read
/// time, not maintained counters — approximate under concurrent writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseView {
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
pub struct DeploymentUniverseCreateParams {
    pub universe_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseCreateResponse {
    pub universe: DeploymentUniverseView,
    /// False when the universe already existed (create is idempotent).
    pub created: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseListResponse {
    #[serde(default)]
    pub universes: Vec<DeploymentUniverseView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseReadParams {
    pub universe_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseReadResponse {
    pub universe: DeploymentUniverseView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseDeleteParams {
    pub universe_id: String,
}

/// Purge report. The purge is idempotent: rerunning after a partial failure
/// resumes where it stopped, and the universe row is deleted last so a
/// half-purged universe is still visible to `deployment/universes/read`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseDeleteResponse {
    pub universe_id: String,
    /// Live session workflows terminated during the purge.
    pub workflows_terminated: u64,
    /// External object-store blobs deleted during the purge.
    pub blob_objects_deleted: u64,
}

/// Non-secret API-key metadata. The owning universe is supplied by every
/// request and intentionally omitted from entries so list responses cannot
/// become a deployment-wide tenant catalog by accident.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentApiKeyView {
    pub key_prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentApiKeyCreateParams {
    pub universe_id: String,
    /// Human-readable purpose shown in key-management interfaces.
    pub display_name: String,
    /// Audit principal applied to grants and flows created through this key.
    /// This does not grant platform/deployment authority.
    pub principal: PrincipalRefView,
}

/// A newly minted key. `secret` is returned only by create and cannot be
/// recovered later. Its custom `Debug` implementation redacts the DTO before
/// JSON-RPC serialization; the serialized response payload remains sensitive
/// and must not be logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentApiKeyCreateResponse {
    pub api_key: DeploymentApiKeyView,
    pub secret: String,
}

impl fmt::Debug for DeploymentApiKeyCreateResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeploymentApiKeyCreateResponse")
            .field("api_key", &self.api_key)
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentApiKeyListParams {
    pub universe_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentApiKeyListResponse {
    #[serde(default)]
    pub api_keys: Vec<DeploymentApiKeyView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentApiKeyRevokeParams {
    pub universe_id: String,
    pub key_prefix: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentApiKeyRevokeResponse {
    pub api_key: DeploymentApiKeyView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DeploymentEnvironmentProviderTransport {
    WebSocket,
    Http,
    Stdio,
    Ssh,
    Provider {
        #[serde(rename = "providerType")]
        provider_type: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentProviderConnection {
    pub endpoint: String,
    pub transport: DeploymentEnvironmentProviderTransport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentProviderView {
    pub provider_id: EnvironmentProviderId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub controller_connection: DeploymentEnvironmentProviderConnection,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentProviderPutParams {
    pub provider_id: EnvironmentProviderId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub controller_connection: DeploymentEnvironmentProviderConnection,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentProviderPutResponse {
    pub provider: DeploymentEnvironmentProviderView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentProviderListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentProviderListResponse {
    pub providers: Vec<DeploymentEnvironmentProviderView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentProviderReadParams {
    pub provider_id: EnvironmentProviderId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentProviderReadResponse {
    pub provider: DeploymentEnvironmentProviderView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentProviderDeleteParams {
    pub provider_id: EnvironmentProviderId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentProviderDeleteResponse {
    pub provider: DeploymentEnvironmentProviderView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentProviderBindingPutParams {
    pub universe_id: String,
    pub binding_id: EnvironmentProviderBindingId,
    pub provider_id: EnvironmentProviderId,
    pub status: EnvironmentProviderBindingStatusView,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentProviderBindingPutResponse {
    pub binding: EnvironmentProviderBindingView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentProviderBindingDeleteParams {
    pub universe_id: String,
    pub binding_id: EnvironmentProviderBindingId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentProviderBindingDeleteResponse {
    pub binding: EnvironmentProviderBindingView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentAdoptParams {
    pub universe_id: String,
    /// Stable caller-generated retry identity inside the destination universe.
    pub request_id: EnvironmentProvisionRequestId,
    pub binding_id: EnvironmentProviderBindingId,
    /// Provider-native source reference. The Incus provider accepts
    /// `<project>/<instance>` or an instance name in the `default` project.
    pub source_target: String,
    /// Required acknowledgement that Lightspeed will move, reconfigure, and
    /// delete the target as part of its ordinary managed lifecycle.
    pub take_ownership: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentAdoptResponse {
    pub environment: EnvironmentView,
}

/// One channel account of any universe, for the connector host's discovery
/// pass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentChannelAccountView {
    pub universe_id: String,
    #[serde(flatten)]
    pub account: ChannelAccountView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentChannelAccountListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ChannelProvider>,
    /// Include disabled accounts; by default only enabled ones are listed.
    #[serde(default)]
    pub include_disabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentChannelAccountListResponse {
    #[serde(default)]
    pub accounts: Vec<DeploymentChannelAccountView>,
}

#[async_trait]
pub trait DeploymentApiService: Send + Sync {
    async fn create_universe(
        &self,
        params: DeploymentUniverseCreateParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseCreateResponse>, AgentApiError>;

    async fn list_universes(
        &self,
        params: DeploymentUniverseListParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseListResponse>, AgentApiError>;

    async fn read_universe(
        &self,
        params: DeploymentUniverseReadParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseReadResponse>, AgentApiError>;

    async fn delete_universe(
        &self,
        params: DeploymentUniverseDeleteParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseDeleteResponse>, AgentApiError>;

    async fn create_api_key(
        &self,
        params: DeploymentApiKeyCreateParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyCreateResponse>, AgentApiError>;

    async fn list_api_keys(
        &self,
        params: DeploymentApiKeyListParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyListResponse>, AgentApiError>;

    async fn revoke_api_key(
        &self,
        params: DeploymentApiKeyRevokeParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyRevokeResponse>, AgentApiError>;

    async fn put_environment_provider(
        &self,
        _params: DeploymentEnvironmentProviderPutParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentProviderPutResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "environment providers are unavailable",
        ))
    }

    async fn list_environment_providers(
        &self,
        _params: DeploymentEnvironmentProviderListParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentProviderListResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "environment providers are unavailable",
        ))
    }

    async fn read_environment_provider(
        &self,
        _params: DeploymentEnvironmentProviderReadParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentProviderReadResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "environment providers are unavailable",
        ))
    }

    async fn delete_environment_provider(
        &self,
        _params: DeploymentEnvironmentProviderDeleteParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentProviderDeleteResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "environment providers are unavailable",
        ))
    }

    async fn put_environment_provider_binding(
        &self,
        _params: DeploymentProviderBindingPutParams,
    ) -> Result<AgentApiOutcome<DeploymentProviderBindingPutResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "environment provider bindings are unavailable",
        ))
    }

    async fn delete_environment_provider_binding(
        &self,
        _params: DeploymentProviderBindingDeleteParams,
    ) -> Result<AgentApiOutcome<DeploymentProviderBindingDeleteResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "environment provider bindings are unavailable",
        ))
    }

    async fn adopt_environment(
        &self,
        _params: DeploymentEnvironmentAdoptParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentAdoptResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "managed environment adoption is unavailable",
        ))
    }

    async fn list_deployment_channel_accounts(
        &self,
        _params: DeploymentChannelAccountListParams,
    ) -> Result<AgentApiOutcome<DeploymentChannelAccountListResponse>, AgentApiError> {
        Err(AgentApiError::internal("channel accounts are unavailable"))
    }
}

macro_rules! deployment_api_methods {
    ($($method_const:ident => $service_fn:ident($params:ty) -> $response:ty =>
        [$summary:expr, $description:expr], access: $access:expr),+ $(,)?) => {
        pub(crate) fn deployment_method_access(method: &str) -> Option<MethodAccess> {
            match method {
                $($method_const => Some($access),)+
                _ => None,
            }
        }

        pub async fn dispatch_deployment_json_rpc(
            service: &dyn DeploymentApiService,
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

        /// One entry per deployment JSON-RPC method, in dispatch order.
        /// Generated by the same macro invocation as the dispatcher, so the
        /// manifest cannot drift from it.
        pub fn deployment_method_manifest() -> Vec<MethodSpec> {
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

deployment_api_methods! {
    METHOD_DEPLOYMENT_UNIVERSES_CREATE => create_universe(DeploymentUniverseCreateParams) -> DeploymentUniverseCreateResponse =>
        ["Create a universe", "Creates the deployment tenant boundary for an explicit UUID. The operation is idempotent and reports whether a new universe was created."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_UNIVERSES_LIST => list_universes(DeploymentUniverseListParams) -> DeploymentUniverseListResponse =>
        ["List universes", "Returns deployment-wide universe summaries with approximate live aggregate counts and last session activity."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_UNIVERSES_READ => read_universe(DeploymentUniverseReadParams) -> DeploymentUniverseReadResponse =>
        ["Read a universe", "Returns one deployment tenant summary with aggregate session, workspace, profile, and blob usage."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_UNIVERSES_DELETE => delete_universe(DeploymentUniverseDeleteParams) -> DeploymentUniverseDeleteResponse =>
        ["Purge a universe", "Permanently terminates live session workflows, deletes external blob objects, and cascades universe data. The purge is resumable/idempotent after partial failure."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_API_KEYS_CREATE => create_api_key(DeploymentApiKeyCreateParams) -> DeploymentApiKeyCreateResponse =>
        ["Create a universe API key", "Mints an inbound gateway key for one existing universe. The plaintext secret is returned exactly once and cannot be recovered; persist only the displayed prefix for identification."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_API_KEYS_LIST => list_api_keys(DeploymentApiKeyListParams) -> DeploymentApiKeyListResponse =>
        ["List universe API keys", "Returns only non-secret key metadata for the requested universe, including revocation and last-use timestamps. Plaintext secrets are never stored or returned."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_API_KEYS_REVOKE => revoke_api_key(DeploymentApiKeyRevokeParams) -> DeploymentApiKeyRevokeResponse =>
        ["Revoke a universe API key", "Immediately and idempotently revokes the matching key only when it belongs to the requested universe. Unknown and foreign-universe prefixes return not found."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_PUT => put_environment_provider(DeploymentEnvironmentProviderPutParams) -> DeploymentEnvironmentProviderPutResponse =>
        ["Put an environment provider", "Registers or replaces one deployment provider and its controller connection. The provider does not call this API or require access to Lightspeed."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_LIST => list_environment_providers(DeploymentEnvironmentProviderListParams) -> DeploymentEnvironmentProviderListResponse =>
        ["List environment providers", "Returns every deployment-registered deployment provider and its controller connection."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_READ => read_environment_provider(DeploymentEnvironmentProviderReadParams) -> DeploymentEnvironmentProviderReadResponse =>
        ["Read an environment provider", "Returns one deployment-registered deployment provider and its controller connection."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_DELETE => delete_environment_provider(DeploymentEnvironmentProviderDeleteParams) -> DeploymentEnvironmentProviderDeleteResponse =>
        ["Delete an environment provider", "Deletes a deployment provider only when no universe binding references it."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_PROVIDER_BINDINGS_PUT => put_environment_provider_binding(DeploymentProviderBindingPutParams) -> DeploymentProviderBindingPutResponse =>
        ["Put an environment provider binding", "Creates or replaces one universe's complete revisioned routing and admission binding. A deployment provider may have at most one binding in a universe."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_PROVIDER_BINDINGS_DELETE => delete_environment_provider_binding(DeploymentProviderBindingDeleteParams) -> DeploymentProviderBindingDeleteResponse =>
        ["Delete an environment provider binding", "Deletes a universe provider binding only after every referencing environment has reached Closed."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_ENVIRONMENTS_ADOPT => adopt_environment(DeploymentEnvironmentAdoptParams) -> DeploymentEnvironmentAdoptResponse =>
        ["Adopt a provider environment", "Creates a universe environment by transferring an existing provider target into Lightspeed's managed lifecycle. The caller must explicitly accept ownership transfer."], access: MethodAccess::DeploymentAdmin,
    METHOD_DEPLOYMENT_CHANNELS_ACCOUNTS_LIST => list_deployment_channel_accounts(DeploymentChannelAccountListParams) -> DeploymentChannelAccountListResponse =>
        ["List channel accounts across universes", "The connector host's discovery call: every enabled provider account of the deployment with its universe id and credential grant reference. Re-poll to pick up accounts created or disabled since."], access: MethodAccess::DeploymentAdminOrCapability(ServiceCapability::DiscoverChannelAccounts),
}
