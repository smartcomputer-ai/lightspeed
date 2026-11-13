//! Deployment-scoped (deployment-level) API contract.
//!
//! Deployment methods address the deployment itself — the set of universes —
//! rather than acting inside one universe, so they form a second scope class
//! with its own service trait and dispatcher. They share the JSON-RPC
//! envelope, error model, and `/rpc` endpoint with the universe-scoped API;
//! the `deployment/` method-name prefix is what routes a request here. Only a
//! deployment key holding the method's group reaches them.

use super::*;
use std::collections::BTreeSet;

/// Method-name prefix that marks the deployment scope. Dispatch keys off this
/// mechanically, so the scope of every method is visible in its name.
pub const DEPLOYMENT_METHOD_PREFIX: &str = "deployment/";

pub const METHOD_DEPLOYMENT_UNIVERSES_CREATE: &str = "deployment/universes/create";
pub const METHOD_DEPLOYMENT_UNIVERSES_LIST: &str = "deployment/universes/list";
pub const METHOD_DEPLOYMENT_UNIVERSES_READ: &str = "deployment/universes/read";
pub const METHOD_DEPLOYMENT_UNIVERSES_HANDLE_PUT: &str = "deployment/universes/handle/put";
pub const METHOD_DEPLOYMENT_UNIVERSES_CONFIGURATION_PUT: &str =
    "deployment/universes/configuration/put";
pub const METHOD_DEPLOYMENT_UNIVERSES_DELETE: &str = "deployment/universes/delete";
pub const METHOD_DEPLOYMENT_PROVIDER_BINDINGS_LIST: &str =
    "deployment/environment-provider-bindings/list";

pub const METHOD_DEPLOYMENT_API_KEYS_CREATE: &str = "deployment/api-keys/create";
pub const METHOD_DEPLOYMENT_API_KEYS_PUT: &str = "deployment/api-keys/put";
pub const METHOD_DEPLOYMENT_API_KEYS_LIST: &str = "deployment/api-keys/list";
pub const METHOD_DEPLOYMENT_API_KEYS_ROTATE: &str = "deployment/api-keys/rotate";
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
    /// The universe's UUID or handle.
    pub universe: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentUniverseReadResponse {
    pub universe: DeploymentUniverseView,
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

/// Patch desired Platform configuration. Absent fields retain their values.
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

/// Non-secret key metadata: what the key is called, reaches and may call,
/// and who minted it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentApiKeyView {
    /// The key's name among the keys of its scope. Together with the scope
    /// it addresses the key, and it is kept when the secret is replaced.
    pub key_id: String,
    /// The first characters of the key's current secret: enough to tell
    /// which secret the key holds without revealing it. It changes when the
    /// secret does.
    pub key_prefix: String,
    pub scope: AccessScope,
    pub groups: Vec<MethodGroup>,
    pub assert_actor: bool,
    pub created_by: Attribution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at_ms: Option<u64>,
}

/// The literal that asks for every method group a key's scope allows.
pub const API_KEY_GROUPS_ALL: &str = "all";

/// The method groups a key is created or declared with: the literal `"all"`,
/// or a non-empty list of groups.
///
/// A key's authority is always stated. `"all"` is every group the key's
/// scope allows in the release that writes the key; the stored key holds
/// that concrete set, so a later release that adds a group never widens it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiKeyGroups {
    /// Every group the key's scope allows.
    All,
    /// Exactly these groups.
    Listed(BTreeSet<MethodGroup>),
}

impl ApiKeyGroups {
    /// The concrete groups a key of `scope` holds.
    pub fn resolve(&self, scope: AccessScope) -> BTreeSet<MethodGroup> {
        match self {
            Self::All => MethodGroup::allowed_in(scope),
            Self::Listed(groups) => groups.clone(),
        }
    }
}

impl FromIterator<MethodGroup> for ApiKeyGroups {
    fn from_iter<I: IntoIterator<Item = MethodGroup>>(groups: I) -> Self {
        Self::Listed(groups.into_iter().collect())
    }
}

impl Serialize for ApiKeyGroups {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::All => serializer.serialize_str(API_KEY_GROUPS_ALL),
            Self::Listed(groups) => groups.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ApiKeyGroups {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct GroupsVisitor;

        impl<'de> de::Visitor<'de> for GroupsVisitor {
            type Value = ApiKeyGroups;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("\"all\" or a non-empty list of method groups")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                if value == API_KEY_GROUPS_ALL {
                    Ok(ApiKeyGroups::All)
                } else {
                    Err(E::invalid_value(de::Unexpected::Str(value), &self))
                }
            }

            fn visit_seq<A: de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut groups = BTreeSet::new();
                while let Some(group) = sequence.next_element::<MethodGroup>()? {
                    groups.insert(group);
                }
                if groups.is_empty() {
                    return Err(de::Error::invalid_length(0, &self));
                }
                Ok(ApiKeyGroups::Listed(groups))
            }
        }

        deserializer.deserialize_any(GroupsVisitor)
    }
}

impl JsonSchema for ApiKeyGroups {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ApiKeyGroups".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let group = generator.subschema_for::<MethodGroup>();
        // The list is refused when empty. The schema leaves that to the
        // description because a minimum length turns the generated client
        // type into a tuple that an ordinary array does not satisfy.
        schemars::json_schema!({
            "description": "The method groups a key is created or declared with: the literal \
                `all`, which is every group the key's scope allows, or a non-empty list of groups.",
            "anyOf": [
                { "type": "string", "const": API_KEY_GROUPS_ALL },
                { "type": "array", "items": group, "uniqueItems": true }
            ]
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentApiKeyCreateParams {
    /// A universe, or the deployment. A deployment key addresses a universe
    /// with the `x-lightspeed-universe` header and may hold deployment groups.
    pub scope: AccessScope,
    /// The key's id: 1-128 ASCII letters, digits, underscores, hyphens, dots
    /// or colons, starting with a letter or digit, and not yet used by a key
    /// of this scope. Omit it to have one allocated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// The method groups the key may call: a non-empty list, or `"all"` for
    /// every group its scope allows. The key stores the resulting set.
    pub groups: ApiKeyGroups,
    /// Whether the key may name the actor a request acts for with the
    /// `x-lightspeed-actor` header. Give it only to a gate that authenticates
    /// people, such as the Platform. It cannot be changed afterwards.
    #[serde(default)]
    pub assert_actor: bool,
    /// Human-readable purpose shown in key-management interfaces.
    pub display_name: String,
}

/// Declare a key whose secret the caller supplies. `Debug` redacts the
/// secret; the serialized request remains sensitive and must not be logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentApiKeyPutParams {
    /// A universe, or the deployment. It cannot be changed afterwards.
    pub scope: AccessScope,
    /// The key's id within its scope: 1-128 ASCII letters, digits,
    /// underscores, hyphens, dots or colons, starting with a letter or digit.
    pub key_id: String,
    /// The method groups the key may call: a non-empty list, or `"all"` for
    /// every group its scope allows. An existing key's groups are replaced
    /// by these, and `"all"` is resolved again on every call, so repeating a
    /// declaration after an upgrade gives the key the groups added since.
    pub groups: ApiKeyGroups,
    /// Whether the key may name the actor a request acts for. It cannot be
    /// changed afterwards.
    #[serde(default)]
    pub assert_actor: bool,
    /// Human-readable purpose shown in key-management interfaces.
    pub display_name: String,
    /// The secret the key answers to: `lsk_` followed by 32 random bytes
    /// encoded as base64url without padding. Generate it with a
    /// cryptographic random source.
    pub secret: String,
}

impl fmt::Debug for DeploymentApiKeyPutParams {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeploymentApiKeyPutParams")
            .field("scope", &self.scope)
            .field("key_id", &self.key_id)
            .field("groups", &self.groups)
            .field("assert_actor", &self.assert_actor)
            .field("display_name", &self.display_name)
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentApiKeyPutResponse {
    pub api_key: DeploymentApiKeyView,
    /// False when the key already existed.
    pub created: bool,
    /// True when an existing key held a different secret, which no longer
    /// works.
    pub secret_replaced: bool,
}

/// A newly minted or rotated key. `secret` is returned only by create or rotate and cannot be
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentApiKeyListParams {
    /// Only keys of this scope; absent lists every key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<AccessScope>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentApiKeyListResponse {
    #[serde(default)]
    pub api_keys: Vec<DeploymentApiKeyView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentApiKeyRotateParams {
    /// The scope the key belongs to.
    pub scope: AccessScope,
    pub key_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentApiKeyRevokeParams {
    /// The scope the key belongs to.
    pub scope: AccessScope,
    pub key_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentApiKeyRevokeResponse {
    pub api_key: DeploymentApiKeyView,
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
    async fn list_deleted_sessions(
        &self,
        _params: DeploymentDeletedSessionsListParams,
    ) -> Result<AgentApiOutcome<DeploymentDeletedSessionsListResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "deleted session administration is unavailable",
        ))
    }
    async fn purge_session(
        &self,
        _params: DeploymentSessionPurgeParams,
    ) -> Result<AgentApiOutcome<DeploymentSessionPurgeResponse>, AgentApiError> {
        Err(AgentApiError::internal(
            "deleted session administration is unavailable",
        ))
    }

    async fn list_deployment_provider_bindings(
        &self,
        _params: DeploymentUniverseReadParams,
    ) -> Result<AgentApiOutcome<EnvironmentProviderBindingListResponse>, AgentApiError> {
        Err(AgentApiError::rejected(
            "deployment binding inventory unavailable",
        ))
    }
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

    async fn put_universe_configuration(
        &self,
        params: DeploymentUniverseConfigurationPutParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseReadResponse>, AgentApiError>;

    async fn put_universe_handle(
        &self,
        params: DeploymentUniverseHandlePutParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseReadResponse>, AgentApiError>;

    async fn delete_universe(
        &self,
        params: DeploymentUniverseDeleteParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseDeleteResponse>, AgentApiError>;

    async fn create_api_key(
        &self,
        params: DeploymentApiKeyCreateParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyCreateResponse>, AgentApiError>;

    async fn put_api_key(
        &self,
        params: DeploymentApiKeyPutParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyPutResponse>, AgentApiError>;

    async fn list_api_keys(
        &self,
        params: DeploymentApiKeyListParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyListResponse>, AgentApiError>;

    async fn rotate_api_key(
        &self,
        params: DeploymentApiKeyRotateParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyCreateResponse>, AgentApiError>;

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
    METHOD_DEPLOYMENT_SESSIONS_DELETED_LIST => list_deleted_sessions(DeploymentDeletedSessionsListParams) -> DeploymentDeletedSessionsListResponse =>
        ["List deleted sessions", "Administrative metadata for soft-deleted sessions, ordered by session ID. Unavailable through universe-scoped methods."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_SESSIONS_PURGE => purge_session(DeploymentSessionPurgeParams) -> DeploymentSessionPurgeResponse =>
        ["Permanently delete a session", "Removes a soft-deleted session and its soft-deleted history subtree. Rejects any selected session that has not been soft-deleted. Releases event, checkpoint and blob references; unreferenced blobs follow normal garbage collection. Does not delete external workspaces, environments, backups or Temporal history."], access: MethodAccess::Deployment,

    METHOD_DEPLOYMENT_PROVIDER_BINDINGS_LIST => list_deployment_provider_bindings(DeploymentUniverseReadParams) -> EnvironmentProviderBindingListResponse =>
        ["List a universe's deployment provider bindings", "Deployment configuration inventory of one universe's provider bindings."], access: MethodAccess::Deployment,

    METHOD_DEPLOYMENT_UNIVERSES_CREATE => create_universe(DeploymentUniverseCreateParams) -> DeploymentUniverseCreateResponse =>
        ["Create a universe", "Creates the deployment tenant boundary for an explicit UUID, under the given handle or one derived from the UUID. The operation is idempotent and reports whether a new universe was created; an existing universe keeps its handle. A handle another universe holds is a conflict."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_UNIVERSES_LIST => list_universes(DeploymentUniverseListParams) -> DeploymentUniverseListResponse =>
        ["List universes", "Returns deployment-wide universe summaries with approximate live aggregate counts and last session activity."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_UNIVERSES_READ => read_universe(DeploymentUniverseReadParams) -> DeploymentUniverseReadResponse =>
        ["Read a universe", "Returns one deployment tenant summary, addressed by UUID or handle, with aggregate session, workspace, profile, and blob usage."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_UNIVERSES_CONFIGURATION_PUT => put_universe_configuration(DeploymentUniverseConfigurationPutParams) -> DeploymentUniverseReadResponse =>
        ["Declare universe presentation and access", "Updates supplied displayName, members and directoryGroupMappings. Omitted fields remain unchanged; empty lists clear declared grants. Platform automatically adopts named universes and enforces declared access independently of manual memberships. Runtime API key authority is unchanged."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_UNIVERSES_HANDLE_PUT => put_universe_handle(DeploymentUniverseHandlePutParams) -> DeploymentUniverseReadResponse =>
        ["Rename a universe", "Changes the deployment-unique handle without changing the universe UUID or resources. The previous handle is released: requests and URLs that use it stop reaching this universe, and another universe may claim it. Claiming another universe's handle returns a conflict. Does not create a universe."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_UNIVERSES_DELETE => delete_universe(DeploymentUniverseDeleteParams) -> DeploymentUniverseDeleteResponse =>
        ["Purge a universe", "Permanently terminates live session workflows, deletes external blob objects, and cascades universe data. The purge is resumable/idempotent after partial failure."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_API_KEYS_CREATE => create_api_key(DeploymentApiKeyCreateParams) -> DeploymentApiKeyCreateResponse =>
        ["Create a scoped API key", "Mints a key for a universe or the deployment with the method groups it may call and whether it may assert actors, under the given key id or an allocated one. The plaintext secret is returned exactly once and cannot be recovered. A key id already used in the scope is a conflict."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_API_KEYS_PUT => put_api_key(DeploymentApiKeyPutParams) -> DeploymentApiKeyPutResponse =>
        ["Declare a scoped API key", "Makes the key of this id and scope exist, answer to exactly the supplied secret, and hold these groups and name. Repeating it changes nothing. A missing key is created; a different secret replaces the previous one at once. Conflict: a changed actor flag, a revoked key, or a secret belonging to another key."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_API_KEYS_LIST => list_api_keys(DeploymentApiKeyListParams) -> DeploymentApiKeyListResponse =>
        ["List scoped API keys", "Returns non-secret key metadata, all keys or those of one scope, including groups, revocation and last-use timestamps. Plaintext secrets are never stored or returned."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_API_KEYS_ROTATE => rotate_api_key(DeploymentApiKeyRotateParams) -> DeploymentApiKeyCreateResponse =>
        ["Rotate a scoped API key", "Atomically replaces an active key's secret with a generated one, immediately rejecting the old secret on subsequent requests. Preserves the key id, scope, groups, actor authority, name, creator and creation time; clears last use. Returns the new secret once. An unknown or revoked key is not found. Already admitted work continues."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_API_KEYS_REVOKE => revoke_api_key(DeploymentApiKeyRevokeParams) -> DeploymentApiKeyRevokeResponse =>
        ["Revoke a scoped API key", "Revokes the key of this id and scope; revoking a revoked key keeps its first revocation time. An unknown key is not found. A revoked key keeps its key id."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_PUT => put_environment_provider(DeploymentEnvironmentProviderPutParams) -> DeploymentEnvironmentProviderPutResponse =>
        ["Put an environment provider", "Registers or replaces a deployment provider: controller endpoint, optional data route base, and optional bearer credential for controller and data routes (write-only: kept when omitted, removed by clearCredential, never returned). The controller is probed: a definitive refusal is invalidRequest, unreachable is accepted. The provider never calls Lightspeed."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_LIST => list_environment_providers(DeploymentEnvironmentProviderListParams) -> DeploymentEnvironmentProviderListResponse =>
        ["List environment providers", "Returns every deployment-registered provider with its controller endpoint, data route base, and whether a credential is stored."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_READ => read_environment_provider(DeploymentEnvironmentProviderReadParams) -> DeploymentEnvironmentProviderReadResponse =>
        ["Read an environment provider", "Returns one deployment-registered provider with its controller endpoint, data route base, and whether a credential is stored."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_DELETE => delete_environment_provider(DeploymentEnvironmentProviderDeleteParams) -> DeploymentEnvironmentProviderDeleteResponse =>
        ["Delete an environment provider", "Deletes a deployment provider only when no universe binding and no non-closed environment references it."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_PROVIDER_BINDINGS_PUT => put_environment_provider_binding(DeploymentProviderBindingPutParams) -> DeploymentProviderBindingPutResponse =>
        ["Put an environment provider binding", "Creates or replaces one universe's binding to a deployment provider. Only an enabled binding admits creates, template listing, and data routes for the universe."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_PROVIDER_BINDINGS_DELETE => delete_environment_provider_binding(DeploymentProviderBindingDeleteParams) -> DeploymentProviderBindingDeleteResponse =>
        ["Delete an environment provider binding", "Deletes a universe's binding to a provider only when the universe has no non-closed environment of that provider."], access: MethodAccess::Deployment,
    METHOD_DEPLOYMENT_CHANNELS_ACCOUNTS_LIST => list_deployment_channel_accounts(DeploymentChannelAccountListParams) -> DeploymentChannelAccountListResponse =>
        ["List channel accounts across universes", "The connector host's discovery call: every enabled provider account of the deployment with its universe id and credential grant reference. Re-poll to pick up accounts created or disabled since."], access: MethodAccess::Deployment,
}
