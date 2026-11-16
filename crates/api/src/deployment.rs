//! Deployment-scoped (deployment-level) API contract.
//!
//! Deployment methods address the deployment itself — the set of universes —
//! rather than acting inside one universe, so they form a second scope class
//! with its own service trait. They share the resource HTTP transport and
//! error model with the universe-scoped API;
//! the `deployment/` method-name prefix is what routes a request here. Every configured
//! runtime token reaches them.

use super::*;

/// Method-name prefix that marks the deployment scope. Dispatch keys off this
/// mechanically, so the scope of every method is visible in its name.
pub const DEPLOYMENT_METHOD_PREFIX: &str = "deployment/";

pub const METHOD_DEPLOYMENT_UNIVERSES_CREATE: &str = "deployment/universes/create";
pub const METHOD_DEPLOYMENT_UNIVERSES_LIST: &str = "deployment/universes/list";
pub const METHOD_DEPLOYMENT_UNIVERSES_READ: &str = "deployment/universes/read";
pub const METHOD_DEPLOYMENT_UNIVERSES_EXPIRY_PUT: &str = "deployment/universes/expiry/put";
pub const METHOD_DEPLOYMENT_UNIVERSES_HANDLE_PUT: &str = "deployment/universes/handle/put";
pub const METHOD_DEPLOYMENT_UNIVERSES_CONFIGURATION_PUT: &str =
    "deployment/universes/configuration/put";
pub const METHOD_DEPLOYMENT_UNIVERSES_DELETED_LIST: &str = "deployment/universes/deleted/list";
pub const METHOD_DEPLOYMENT_UNIVERSES_DELETE: &str = "deployment/universes/delete";
pub const METHOD_DEPLOYMENT_PROVIDER_BINDINGS_LIST: &str =
    "deployment/environment-provider-bindings/list";

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
pub const METHOD_DEPLOYMENT_CHANNELS_ACCOUNTS_LIST: &str = "deployment/channels/accounts/list";

pub const METHOD_DEPLOYMENT_SESSIONS_DELETED_LIST: &str = "deployment/sessions/deleted/list";
pub const METHOD_DEPLOYMENT_SESSIONS_PURGE: &str = "deployment/sessions/purge";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentDeletedSessionsListParams {
    /// The universe's UUID or handle.
    pub universe: String,
    /// Exclusive session ID cursor; each page contains at most 100 entries.
    #[serde(default)]
    pub after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeletedSessionView {
    pub session_id: String,
    pub display_name: Option<String>,
    pub deleted_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentDeletedSessionsListResponse {
    pub sessions: Vec<DeletedSessionView>,
    pub next_after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentSessionPurgeParams {
    /// The universe's UUID or handle.
    pub universe: String,
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentSessionPurgeResponse {
    /// Includes soft-deleted delegated descendants.
    /// Empty when the target has already been permanently deleted.
    pub deleted_session_ids: Vec<String>,
}

pub fn is_deployment_method(method: &str) -> bool {
    method.starts_with(DEPLOYMENT_METHOD_PREFIX)
}

/// Per-universe stats view. Counts are cheap aggregates computed at read
/// time, not maintained counters — approximate under concurrent writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseView {
    #[serde(flatten)]
    pub configuration: crate::UniverseConfiguration,
    pub universe_id: String,
    /// The universe's deployment-unique name. Requests may address the
    /// universe by it wherever they may by UUID.
    pub handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    pub created_at_ms: u64,
    /// Most recent session activity (`max(sessions.updated_at_ms)`); absent
    /// when the universe has no sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at_ms: Option<u64>,
    pub sessions: u64,
    pub workspaces: u64,
    pub profiles: u64,
    pub blob_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletion: Option<UniverseDeletion>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseCreateParams {
    /// At most 32 string entries; keys up to 64 bytes, values up to 256 bytes.
    /// Caller keys cannot start with lightspeed.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub protected: bool,
    /// Future Unix time in milliseconds. Incompatible with protection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    pub universe_id: String,
    /// The new universe's handle: 1-63 lowercase letters, digits and
    /// hyphens, starting and ending with a letter or digit, and not in the
    /// form of a UUID. Omit it to have one derived from the UUID. Ignored
    /// when the universe already exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
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
pub struct DeploymentUniverseListParams {
    /// Return universes containing every supplied key/value pair.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseListResponse {
    #[serde(default)]
    pub universes: Vec<DeploymentUniverseView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseReadParams {
    /// The universe's UUID or handle.
    pub universe: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseReadResponse {
    pub universe: DeploymentUniverseView,
}

/// Replace or clear expiry. Explicit null clears it; omission is refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentUniverseExpiryPutParams {
    pub universe: String,
    #[serde(deserialize_with = "deserialize_universe_expiry")]
    #[schemars(required, schema_with = "universe_expiry_schema")]
    pub expires_at_ms: Option<u64>,
}
fn deserialize_universe_expiry<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(deserializer)
}
fn universe_expiry_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":["integer","null"],"format":"uint64","minimum":1,"maximum":9223372036854775807_u64})
}

/// Rename a universe. The UUID and resources remain unchanged; the previous
/// handle is released, so requests and URLs using it stop reaching this
/// universe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseHandlePutParams {
    /// The universe's UUID or current handle.
    pub universe: String,
    /// The new handle: 1-63 lowercase letters, digits and hyphens, starting
    /// and ending with a letter or digit, and not in the form of a UUID.
    pub handle: String,
}

/// Patch universe settings. Absent fields retain their values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseConfigurationPutParams {
    pub universe: String,
    #[serde(flatten)]
    pub configuration: crate::UniverseConfiguration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseDeleteParams {
    /// The universe's UUID or handle.
    pub universe: String,
}

/// Current state after the immediate deletion fence has been applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum UniverseDeletionState {
    Deleting,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum UniverseDeletionStep {
    Intake,
    Workflows,
    Environments,
    Storage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UniverseDeletion {
    pub started_at_ms: u64,
    pub requested_by: Attribution,
    pub step: UniverseDeletionStep,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseDeleteResponse {
    pub universe_id: String,
    pub state: UniverseDeletionState,
}

/// Permanent resource inventory and attribution for a deleted universe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeletedUniverseView {
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    pub universe_id: String,
    pub handle: String,
    pub display_name: Option<String>,
    pub created_at_ms: u64,
    pub deleting_at_ms: u64,
    pub deleted_at_ms: u64,
    pub requested_by: Attribution,
    pub summary: serde_json::Value,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentDeletedUniversesListParams {
    /// Opaque exclusive cursor; each page contains at most 100 entries.
    #[serde(default)]
    pub after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentDeletedUniversesListResponse {
    pub universes: Vec<DeletedUniverseView>,
    pub next_after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentProviderView {
    pub provider_id: EnvironmentProviderId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// `ws://`, `wss://` or `unix://` URL of the provider controller.
    pub controller_endpoint: String,
    /// Operator-configured `ws://` or `wss://` base of the provider's data
    /// routes; absent derives it from the controller endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_route_base: Option<String>,
    /// The data route base Lightspeed dials: `dataRouteBase`, or the
    /// controller endpoint's origin followed by `/routes`. Absent only when it
    /// cannot be derived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_data_route_base: Option<String>,
    /// A bearer credential is stored and presented to the controller and
    /// data routes; without one no `Authorization` header is sent.
    pub has_credential: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Register or replace a provider. `credential` is the optional bearer
/// token Lightspeed presents to the provider's controller and data routes:
/// it is sealed on receipt and never returned by any method. `Debug` output
/// redacts it; request logging must never echo these params.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentEnvironmentProviderPutParams {
    pub provider_id: EnvironmentProviderId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// `ws://`, `wss://` or `unix://` URL of the provider controller.
    pub controller_endpoint: String,
    /// `ws://` or `wss://` base of the provider's data routes; omit to derive
    /// it from the controller endpoint. Required for a `unix://` controller,
    /// and must not be `ws://` for a `wss://` controller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_route_base: Option<String>,
    /// A new bearer credential; omit to keep the stored one. A provider needs
    /// none. Excludes `clearCredential`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    /// Remove the stored credential. Excludes `credential`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear_credential: bool,
}

impl fmt::Debug for DeploymentEnvironmentProviderPutParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeploymentEnvironmentProviderPutParams")
            .field("provider_id", &self.provider_id)
            .field("display_name", &self.display_name)
            .field("controller_endpoint", &self.controller_endpoint)
            .field("data_route_base", &self.data_route_base)
            .field(
                "credential",
                &self.credential.as_ref().map(|_| "<redacted>"),
            )
            .field("clear_credential", &self.clear_credential)
            .finish()
    }
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

/// Create or replace a universe's binding to a deployment provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentProviderBindingPutParams {
    /// The universe's UUID or handle.
    pub universe: String,
    pub provider_id: EnvironmentProviderId,
    /// Admits creates, template listing and data routes for the universe.
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentProviderBindingPutResponse {
    pub binding: EnvironmentProviderBindingView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentProviderBindingDeleteParams {
    /// The universe's UUID or handle.
    pub universe: String,
    pub provider_id: EnvironmentProviderId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentProviderBindingDeleteResponse {
    pub binding: EnvironmentProviderBindingView,
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
    async fn list_deleted_universes(
        &self,
        _params: DeploymentDeletedUniversesListParams,
    ) -> Result<DeploymentDeletedUniversesListResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "deleted universe administration is unavailable",
        ))
    }

    async fn list_deleted_sessions(
        &self,
        _params: DeploymentDeletedSessionsListParams,
    ) -> Result<DeploymentDeletedSessionsListResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "deleted session administration is unavailable",
        ))
    }
    async fn purge_session(
        &self,
        _params: DeploymentSessionPurgeParams,
    ) -> Result<DeploymentSessionPurgeResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "deleted session administration is unavailable",
        ))
    }

    async fn list_deployment_provider_bindings(
        &self,
        _params: DeploymentUniverseReadParams,
    ) -> Result<EnvironmentProviderBindingListResponse, AgentApiError> {
        Err(AgentApiError::rejected(
            "deployment binding inventory unavailable",
        ))
    }
    async fn create_universe(
        &self,
        params: DeploymentUniverseCreateParams,
    ) -> Result<DeploymentUniverseCreateResponse, AgentApiError>;

    async fn list_universes(
        &self,
        params: DeploymentUniverseListParams,
    ) -> Result<DeploymentUniverseListResponse, AgentApiError>;

    async fn read_universe(
        &self,
        params: DeploymentUniverseReadParams,
    ) -> Result<DeploymentUniverseReadResponse, AgentApiError>;

    async fn put_universe_configuration(
        &self,
        params: DeploymentUniverseConfigurationPutParams,
    ) -> Result<DeploymentUniverseReadResponse, AgentApiError>;

    async fn put_universe_expiry(
        &self,
        params: DeploymentUniverseExpiryPutParams,
    ) -> Result<DeploymentUniverseReadResponse, AgentApiError>;

    async fn put_universe_handle(
        &self,
        params: DeploymentUniverseHandlePutParams,
    ) -> Result<DeploymentUniverseReadResponse, AgentApiError>;

    async fn delete_universe(
        &self,
        params: DeploymentUniverseDeleteParams,
    ) -> Result<DeploymentUniverseDeleteResponse, AgentApiError>;

    async fn put_environment_provider(
        &self,
        _params: DeploymentEnvironmentProviderPutParams,
    ) -> Result<DeploymentEnvironmentProviderPutResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "environment providers are unavailable",
        ))
    }

    async fn list_environment_providers(
        &self,
        _params: DeploymentEnvironmentProviderListParams,
    ) -> Result<DeploymentEnvironmentProviderListResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "environment providers are unavailable",
        ))
    }

    async fn read_environment_provider(
        &self,
        _params: DeploymentEnvironmentProviderReadParams,
    ) -> Result<DeploymentEnvironmentProviderReadResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "environment providers are unavailable",
        ))
    }

    async fn delete_environment_provider(
        &self,
        _params: DeploymentEnvironmentProviderDeleteParams,
    ) -> Result<DeploymentEnvironmentProviderDeleteResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "environment providers are unavailable",
        ))
    }

    async fn put_environment_provider_binding(
        &self,
        _params: DeploymentProviderBindingPutParams,
    ) -> Result<DeploymentProviderBindingPutResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "environment provider bindings are unavailable",
        ))
    }

    async fn delete_environment_provider_binding(
        &self,
        _params: DeploymentProviderBindingDeleteParams,
    ) -> Result<DeploymentProviderBindingDeleteResponse, AgentApiError> {
        Err(AgentApiError::internal(
            "environment provider bindings are unavailable",
        ))
    }

    async fn list_deployment_channel_accounts(
        &self,
        _params: DeploymentChannelAccountListParams,
    ) -> Result<DeploymentChannelAccountListResponse, AgentApiError> {
        Err(AgentApiError::internal("channel accounts are unavailable"))
    }
}

macro_rules! deployment_api_methods {
    ($($method_const:path => $service_fn:ident($params:ty) -> $response:ty =>
        [$summary:expr, $description:expr], access: $access:expr,
        http: ($verb:ident, $path:literal, [$(($parameter:literal, $field:literal)),*], [$($query:literal),*], $transfer:ident, $success:ident)),+ $(,)?) => {
        pub(crate) fn deployment_method_access(method: &str) -> Option<MethodAccess> {
            match method {
                $($method_const => Some($access),)+
                _ => None,
            }
        }


        /// One entry per deployment operation in declaration order.
        /// Generated by the same macro invocation as the dispatcher, so the
        /// manifest cannot drift from it.
        pub fn deployment_method_manifest() -> Vec<MethodSpec> {
            vec![
                $(
                    MethodSpec {
                        method: $method_const,
                        notifications: &[],
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
macro_rules! for_each_deployment_operation {
    ($consumer:ident) => {
        $consumer! {
    $crate::METHOD_DEPLOYMENT_UNIVERSES_DELETED_LIST => list_deleted_universes($crate::DeploymentDeletedUniversesListParams) -> $crate::DeploymentDeletedUniversesListResponse =>
        ["List deleted universes", "Permanent deletion records, newest first, with resource counts and unconfirmed provider targets."], access: $crate::MethodAccess::Deployment,
        http: (GET, "/api/v1/deleted-universes", [], ["after"], Json, Ok),

    $crate::METHOD_DEPLOYMENT_SESSIONS_DELETED_LIST => list_deleted_sessions($crate::DeploymentDeletedSessionsListParams) -> $crate::DeploymentDeletedSessionsListResponse =>
        ["List deleted sessions", "Administrative metadata for soft-deleted sessions, ordered by session ID. Unavailable through universe-scoped methods."], access: $crate::MethodAccess::Deployment,
        http: (GET, "/api/v1/universes/{universe}/deleted-sessions", [("universe", "universe")], ["after"], Json, Ok),
    $crate::METHOD_DEPLOYMENT_SESSIONS_PURGE => purge_session($crate::DeploymentSessionPurgeParams) -> $crate::DeploymentSessionPurgeResponse =>
        ["Permanently delete a session", "Removes a soft-deleted session and its soft-deleted history subtree. Rejects any selected session that has not been soft-deleted. Releases event, checkpoint and blob references; unreferenced blobs follow normal garbage collection. Does not delete external workspaces, environments, backups or Temporal history."], access: $crate::MethodAccess::Deployment,
        http: (DELETE, "/api/v1/universes/{universe}/deleted-sessions/{sessionId}", [("universe", "universe"), ("sessionId", "sessionId")], [], Json, Ok),

    $crate::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_LIST => list_deployment_provider_bindings($crate::DeploymentUniverseReadParams) -> $crate::EnvironmentProviderBindingListResponse =>
        ["List a universe's deployment provider bindings", "Deployment configuration inventory of one universe's provider bindings."], access: $crate::MethodAccess::Deployment,
        http: (GET, "/api/v1/universes/{universe}/deployment-provider-bindings", [("universe", "universe")], [], Json, Ok),

    $crate::METHOD_DEPLOYMENT_UNIVERSES_CREATE => create_universe($crate::DeploymentUniverseCreateParams) -> $crate::DeploymentUniverseCreateResponse =>
        ["Create a universe", "Creates a universe for an explicit UUID with optional handle, metadata, protection and future expiry. Protection and expiry are mutually exclusive. Idempotent: existing universes retain all settings. A claimed handle or deleting/deleted UUID returns conflict."], access: $crate::MethodAccess::Deployment,
        http: (POST, "/api/v1/universes", [], [], Json, CreatedFlag),
    $crate::METHOD_DEPLOYMENT_UNIVERSES_LIST => list_universes($crate::DeploymentUniverseListParams) -> $crate::DeploymentUniverseListResponse =>
        ["List universes", "Returns deployment-wide summaries, including metadata, protection, expiry and deleting universes. The metadata filter requires every supplied key/value pair. Aggregate counts and last session activity are approximate."], access: $crate::MethodAccess::Deployment,
        http: (GET, "/api/v1/universes", [], ["metadata"], Json, Ok),
    $crate::METHOD_DEPLOYMENT_UNIVERSES_READ => read_universe($crate::DeploymentUniverseReadParams) -> $crate::DeploymentUniverseReadResponse =>
        ["Read a universe", "Returns one deployment tenant summary, addressed by UUID or active handle, with aggregate session, workspace, profile, and blob usage. Deleting universes remain readable by UUID. A permanently deleted UUID returns gone with its deletion record; an unknown UUID returns notFound."], access: $crate::MethodAccess::Deployment,
        http: (GET, "/api/v1/universes/{universe}", [("universe", "universe")], [], Json, Ok),
    $crate::METHOD_DEPLOYMENT_UNIVERSES_CONFIGURATION_PUT => put_universe_configuration($crate::DeploymentUniverseConfigurationPutParams) -> $crate::DeploymentUniverseReadResponse =>
        ["Update universe configuration", "Updates supplied displayName, metadata and protected. Omitted fields retain their values; metadata replaces the complete map. Protection refuses deletion and cannot coexist with an expiry. Clearing protection is explicit and never implied by deletion."], access: $crate::MethodAccess::Deployment,
        http: (PUT, "/api/v1/universes/{universe}/configuration", [("universe", "universe")], [], Json, Ok),
    $crate::METHOD_DEPLOYMENT_UNIVERSES_EXPIRY_PUT => put_universe_expiry($crate::DeploymentUniverseExpiryPutParams) -> $crate::DeploymentUniverseReadResponse =>
        ["Set or clear universe expiry", "Sets a future Unix timestamp or clears expiry with explicit null. Protected universes cannot have an expiry. Expiry starts the normal deletion process, attributed to the runtime; activity does not extend it."], access: $crate::MethodAccess::Deployment,
        http: (PUT, "/api/v1/universes/{universe}/expiry", [("universe", "universe")], [], Json, Ok),
    $crate::METHOD_DEPLOYMENT_UNIVERSES_HANDLE_PUT => put_universe_handle($crate::DeploymentUniverseHandlePutParams) -> $crate::DeploymentUniverseReadResponse =>
        ["Rename a universe", "Changes the deployment-unique handle without changing the universe UUID or resources. The previous handle is released: requests and URLs that use it stop reaching this universe, and another universe may claim it. Claiming another universe's handle returns a conflict. Does not create a universe."], access: $crate::MethodAccess::Deployment,
        http: (PUT, "/api/v1/universes/{universe}/handle", [("universe", "universe")], [], Json, Ok),
    $crate::METHOD_DEPLOYMENT_UNIVERSES_DELETE => delete_universe($crate::DeploymentUniverseDeleteParams) -> $crate::DeploymentUniverseDeleteResponse =>
        ["Request universe deletion", "Refuses protected universes. Immediately fences an unprotected universe and releases its handle. A background worker removes schedules, workflows, environments, objects and rows. Repeated requests return deleting or deleted; unknown IDs return notFound."], access: $crate::MethodAccess::Deployment,
        http: (DELETE, "/api/v1/universes/{universe}", [("universe", "universe")], [], Json, Ok),
    $crate::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_PUT => put_environment_provider($crate::DeploymentEnvironmentProviderPutParams) -> $crate::DeploymentEnvironmentProviderPutResponse =>
        ["Put an environment provider", "Registers or replaces a deployment provider: controller endpoint, optional data route base, and optional bearer credential for controller and data routes (write-only: kept when omitted, removed by clearCredential, never returned). The controller is probed: a definitive refusal is invalidRequest, unreachable is accepted. The provider never calls Lightspeed."], access: $crate::MethodAccess::Deployment,
        http: (PUT, "/api/v1/environment-providers/{providerId}", [("providerId", "providerId")], [], Json, Ok),
    $crate::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_LIST => list_environment_providers($crate::DeploymentEnvironmentProviderListParams) -> $crate::DeploymentEnvironmentProviderListResponse =>
        ["List environment providers", "Returns every deployment-registered provider with its controller endpoint, data route base, and whether a credential is stored."], access: $crate::MethodAccess::Deployment,
        http: (GET, "/api/v1/environment-providers", [], [], Json, Ok),
    $crate::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_READ => read_environment_provider($crate::DeploymentEnvironmentProviderReadParams) -> $crate::DeploymentEnvironmentProviderReadResponse =>
        ["Read an environment provider", "Returns one deployment-registered provider with its controller endpoint, data route base, and whether a credential is stored."], access: $crate::MethodAccess::Deployment,
        http: (GET, "/api/v1/environment-providers/{providerId}", [("providerId", "providerId")], [], Json, Ok),
    $crate::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_DELETE => delete_environment_provider($crate::DeploymentEnvironmentProviderDeleteParams) -> $crate::DeploymentEnvironmentProviderDeleteResponse =>
        ["Delete an environment provider", "Deletes a deployment provider only when no universe binding and no non-closed environment references it."], access: $crate::MethodAccess::Deployment,
        http: (DELETE, "/api/v1/environment-providers/{providerId}", [("providerId", "providerId")], [], Json, Ok),
    $crate::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_PUT => put_environment_provider_binding($crate::DeploymentProviderBindingPutParams) -> $crate::DeploymentProviderBindingPutResponse =>
        ["Put an environment provider binding", "Creates or replaces one universe's binding to a deployment provider. Only an enabled binding admits creates, template listing, and data routes for the universe."], access: $crate::MethodAccess::Deployment,
        http: (PUT, "/api/v1/universes/{universe}/deployment-provider-bindings/{providerId}", [("universe", "universe"), ("providerId", "providerId")], [], Json, Ok),
    $crate::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_DELETE => delete_environment_provider_binding($crate::DeploymentProviderBindingDeleteParams) -> $crate::DeploymentProviderBindingDeleteResponse =>
        ["Delete an environment provider binding", "Deletes a universe's binding to a provider only when the universe has no non-closed environment of that provider."], access: $crate::MethodAccess::Deployment,
        http: (DELETE, "/api/v1/universes/{universe}/deployment-provider-bindings/{providerId}", [("universe", "universe"), ("providerId", "providerId")], [], Json, Ok),
    $crate::METHOD_DEPLOYMENT_CHANNELS_ACCOUNTS_LIST => list_deployment_channel_accounts($crate::DeploymentChannelAccountListParams) -> $crate::DeploymentChannelAccountListResponse =>
        ["List channel accounts across universes", "The connector host's discovery call: every enabled provider account of the deployment with its universe id and credential grant reference. Re-poll to pick up accounts created or disabled since."], access: $crate::MethodAccess::Deployment,
        http: (GET, "/api/v1/deployment/channels/accounts", [], ["includeDisabled", "provider"], Json, Ok),
        }
    };
}

for_each_deployment_operation!(deployment_api_methods);
