use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentActivateParams {
    pub session_id: SessionId,
    pub environment_id: EnvironmentId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentActivateResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentDeactivateParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentDeactivateResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCreateParams {
    /// Stable caller-generated retry identity, unique inside the universe.
    /// Retrying with identical inputs returns the same environment.
    pub request_id: EnvironmentProvisionRequestId,
    /// The provider to create the environment with; the universe needs an
    /// enabled binding to it.
    pub provider_id: EnvironmentProviderId,
    /// The provider template to create the environment from.
    pub template_id: EnvironmentTemplateId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Idle policy passed to the provider in place of the template's `idle`.
    /// The provider runs the idle clock; it cannot be changed after create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle: Option<EnvironmentIdlePolicyView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCreateResponse {
    pub environment: EnvironmentView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentReadParams {
    pub environment_id: EnvironmentId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentReadResponse {
    pub environment: EnvironmentView,
}

/// Replace an environment's display name and caller metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentUpdateParams {
    pub environment_id: EnvironmentId,
    /// The new display name; omit to clear it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The complete new caller metadata; omit or `{}` to clear it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentUpdateResponse {
    pub environment: EnvironmentView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentListParams {
    /// Only provisioned environments of this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<EnvironmentProviderId>,
    /// Only registered environments admitted by this registration key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_key_id: Option<EnvironmentRegistrationKeyId>,
    /// Only environments in this lifecycle state. `closed` lists closed
    /// environments without `includeClosed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<EnvironmentStateView>,
    /// Also list closed environments; they are omitted by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_closed: bool,
    /// Only environments carrying every listed metadata pair (AND
    /// semantics); the same filter `session/list` accepts.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Most environments per page; omit for 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: Option<u32>,
    /// Exclusive environment id cursor: a previous page's `nextAfter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<EnvironmentId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentListResponse {
    /// Environments in environment id order.
    #[serde(default)]
    pub environments: Vec<EnvironmentView>,
    /// Pass as `after` to read the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<EnvironmentId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCloseParams {
    pub environment_id: EnvironmentId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCloseResponse {
    pub environment: EnvironmentView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentIngressPutParams {
    pub environment_id: EnvironmentId,
    /// Ensure (`true`) or remove (`false`) the provider's public ingress.
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentIngressPutResponse {
    pub environment: EnvironmentView,
}

/// Power state of a provisioned environment's target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentPowerStateView {
    Running,
    /// Execution frozen with RAM resident; resume is near-instant.
    Paused,
    /// Execution state saved to disk; resume restores it.
    Suspended,
    /// Powered off with disk retained; resume is a fresh boot.
    Stopped,
}

/// Staged idle policy the provider applies. Thresholds are milliseconds of
/// envd-reported idle time (running processes, jobs and leftover process
/// groups hold it at zero) and must be non-decreasing in the order pause,
/// suspend, stop, close. Stages whose power state the template does not
/// support are skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentIdlePolicyView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspend_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_after_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentPowerPutParams {
    pub environment_id: EnvironmentId,
    /// Power state to move the target to; one of its template's
    /// `powerStates`.
    pub power: EnvironmentPowerStateView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentPowerPutResponse {
    pub environment: EnvironmentView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentExternalCreateParams {
    /// Stable caller-generated retry identity, unique inside the universe.
    pub request_id: EnvironmentProvisionRequestId,
    /// `ws://`, `wss://` or `unix://` URL of an envd instance reachable from
    /// Lightspeed.
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentExternalCreateResponse {
    pub environment: EnvironmentView,
}

/// The lifecycle Lightspeed owns. External and registered environments are
/// only ever `open` or `closed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentStateView {
    /// The provider is creating the target; the environment opens after
    /// Lightspeed's first handshake with its envd.
    Provisioning,
    Open,
    /// The provider is removing the target.
    Closing,
    Closed,
    /// Provisioning or the provider failed; `stateMessage` says why.
    Failed,
}

/// Display status derived from `state` and the latest observation; never
/// stored. Tools that use a paused, suspended or stopped environment wake
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentStatusView {
    Provisioning,
    Ready,
    /// Execution frozen with RAM resident.
    Paused,
    /// Execution state saved to disk.
    Suspended,
    /// Powered off with disk retained.
    Stopped,
    /// Open, but the provider's latest observation is inconclusive.
    Unknown,
    /// Registered, open, and its daemon is not connected.
    Away,
    Closing,
    Closed,
    Failed,
}

/// Why an environment is closing or closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentCloseReasonView {
    /// `environments/close`.
    Requested,
    /// Its registration key was revoked with `closeEnvironments`.
    KeyRevoked,
    /// Its ephemeral daemon stayed away longer than the key's disconnect
    /// grace.
    EphemeralGrace,
    /// Its ephemeral daemon said goodbye.
    Departed,
    /// Its persistent daemon stayed away longer than the key's
    /// `offlineCloseAfterMs`.
    OfflineTimeout,
    /// The provider reported the target closed or gone.
    ProviderEnded,
}

/// Lifecycle phase of a provider target, as the provider reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentTargetPhaseView {
    Provisioning,
    Active,
    Closing,
    Closed,
    Failed,
    Unknown,
}

/// What the provider last reported about a provisioned environment's
/// target. Display only: Lightspeed never converges it, and it may be up to
/// a few minutes old.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentObservedTargetView {
    pub phase: EnvironmentTargetPhaseView,
    /// Present only while `phase` is `active`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<EnvironmentPowerStateView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub observed_at_ms: i64,
}

/// The envd build serving an environment, from its registration or
/// Lightspeed's open handshake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentDaemonView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    pub protocol_version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EnvironmentSourceView {
    /// A target a deployment provider created from a template. The provider
    /// owns its power, idle policy and ingress.
    Provisioned {
        provider_id: EnvironmentProviderId,
        template_id: EnvironmentTemplateId,
        /// The provider's handle for the target; absent until the provider
        /// accepted the create.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<EnvironmentTargetId>,
        /// The create-time idle policy, when one replaced the template's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idle: Option<EnvironmentIdlePolicyView>,
        /// Absent until the provider first reported the target.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed: Option<EnvironmentObservedTargetView>,
        /// The public endpoint the provider last reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        public_endpoint: Option<String>,
    },
    External {
        /// `ws://`, `wss://` or `unix://` URL Lightspeed dials envd at.
        endpoint: String,
    },
    /// An envd that dialed the gateway outbound and was admitted by a
    /// registration key. The key is the environment's group; the daemon id
    /// is derived from the daemon's public key and is its identity.
    Registered {
        registration_key_id: EnvironmentRegistrationKeyId,
        daemon_id: EnvironmentDaemonId,
        /// The admitting key's identity mode.
        identity_mode: EnvironmentIdentityModeView,
        /// The daemon's control connection is up.
        connected: bool,
        /// When the daemon last connected or disconnected.
        connection_changed_at_ms: i64,
        /// When the gateway last recorded the daemon's control connection:
        /// at connect, at disconnect, and at most every five minutes while
        /// connected.
        last_seen_at_ms: i64,
    },
}

/// What happens to a registered environment while its daemon is
/// disconnected. Policy of the registration key that admitted it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentIdentityModeView {
    /// Stays open: tools wait up to two minutes for the daemon to reconnect.
    /// Closed explicitly, or after the key's `offlineCloseAfterMs`.
    Persistent,
    /// Unusable while disconnected; closed once the daemon has been away
    /// longer than the key's disconnect grace, or at once when it says
    /// goodbye.
    Ephemeral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentRegistrationKeyStatusView {
    Active,
    Revoked,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyView {
    pub registration_key_id: EnvironmentRegistrationKeyId,
    /// The group name shown wherever registered environments are listed.
    pub display_name: String,
    /// First characters of the key's live secret, for identification only.
    pub key_prefix: String,
    pub identity_mode: EnvironmentIdentityModeView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active_environments: Option<u32>,
    /// Effective disconnect grace for ephemeral environments.
    pub ephemeral_disconnect_grace_ms: u64,
    /// Persistent keys only: environments whose daemon has been away longer
    /// than this are closed; absent keeps them open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_close_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    pub status: EnvironmentRegistrationKeyStatusView,
    /// Environments ever admitted by this key, closed included.
    pub registered_environment_count: u64,
    /// Non-closed environments admitted by this key.
    pub active_environment_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_registered_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ms: Option<i64>,
}

/// A freshly minted registration secret. `Debug` output is redacted so the
/// plaintext cannot leak through derived logging; it is shown once.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct EnvironmentRegistrationSecretView(pub String);

impl fmt::Debug for EnvironmentRegistrationSecretView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EnvironmentRegistrationSecretView(<redacted>)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyCreateParams {
    /// Group name for the environments this key admits.
    pub display_name: String,
    /// Immutable once the key exists.
    pub identity_mode: EnvironmentIdentityModeView,
    /// Non-closed environments the key may have at once; omit for unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active_environments: Option<u32>,
    /// Disconnect grace for ephemeral environments; omit for the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_disconnect_grace_ms: Option<u64>,
    /// Persistent keys only: close environments whose daemon has been away
    /// longer than this; omit to keep them open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_close_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyCreateResponse {
    pub registration_key: EnvironmentRegistrationKeyView,
    /// The plaintext secret, returned only here.
    pub secret: EnvironmentRegistrationSecretView,
}

/// The complete editable policy of a registration key; an absent field is
/// unset. The identity mode is immutable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyUpdateParams {
    pub registration_key_id: EnvironmentRegistrationKeyId,
    /// Group name for the environments this key admits.
    pub display_name: String,
    /// Non-closed environments the key may have at once; omit for unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active_environments: Option<u32>,
    /// Disconnect grace for ephemeral environments; omit for the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_disconnect_grace_ms: Option<u64>,
    /// Persistent keys only: close environments whose daemon has been away
    /// longer than this; omit to keep them open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_close_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyUpdateResponse {
    pub registration_key: EnvironmentRegistrationKeyView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyRotateParams {
    pub registration_key_id: EnvironmentRegistrationKeyId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyRotateResponse {
    pub registration_key: EnvironmentRegistrationKeyView,
    /// The new plaintext secret, returned only here.
    pub secret: EnvironmentRegistrationSecretView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyReadParams {
    pub registration_key_id: EnvironmentRegistrationKeyId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyReadResponse {
    pub registration_key: EnvironmentRegistrationKeyView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyListResponse {
    #[serde(default)]
    pub registration_keys: Vec<EnvironmentRegistrationKeyView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyRevokeParams {
    pub registration_key_id: EnvironmentRegistrationKeyId,
    /// Also close every non-closed environment the key admitted.
    #[serde(default)]
    pub close_environments: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRegistrationKeyRevokeResponse {
    pub registration_key: EnvironmentRegistrationKeyView,
    /// Environments closed by this call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed_environment_ids: Vec<EnvironmentId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentView {
    pub environment_id: EnvironmentId,
    /// The caller's retry identity; absent for registered environments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<EnvironmentProvisionRequestId>,
    pub source: EnvironmentSourceView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub state: EnvironmentStateView,
    pub status: EnvironmentStatusView,
    /// Why the environment failed, or what provisioning or closing is
    /// waiting on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_message: Option<String>,
    /// Present exactly while `state` is `closing` or `closed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<EnvironmentCloseReasonView>,
    /// When `state` last changed.
    pub state_changed_at_ms: i64,
    /// The envd build serving the environment; absent until known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<EnvironmentDaemonView>,
    /// Caller metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at_ms: Option<i64>,
}

/// A universe's binding to a deployment provider. Creates, templates and
/// data routes of the provider need it enabled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderBindingView {
    pub provider_id: EnvironmentProviderId,
    pub enabled: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderBindingListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderBindingListResponse {
    pub bindings: Vec<EnvironmentProviderBindingView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderBindingReadParams {
    pub provider_id: EnvironmentProviderId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderBindingReadResponse {
    pub binding: EnvironmentProviderBindingView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTemplateView {
    pub template_id: EnvironmentTemplateId,
    pub provider_id: EnvironmentProviderId,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `environments/ingress/put` can enable public ingress on targets of
    /// this template.
    pub public_ingress: bool,
    pub deprecated: bool,
    /// Compute targets of this template get; absent when the provider does
    /// not fix it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<EnvironmentTemplateResourcesView>,
    /// Power states `environments/power/put` can move targets of this
    /// template to; empty without power control.
    pub power_states: Vec<EnvironmentPowerStateView>,
    /// Idle policy targets get when `environments/create` names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle: Option<EnvironmentIdlePolicyView>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

/// A template's compute, in provider-neutral units. Each field is absent
/// when the provider does not fix it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTemplateResourcesView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcpus: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTemplateListParams {
    /// Only this provider's templates; omit for every enabled binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<EnvironmentProviderId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTemplateListResponse {
    pub templates: Vec<EnvironmentTemplateView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTemplateReadParams {
    pub provider_id: EnvironmentProviderId,
    pub template_id: EnvironmentTemplateId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTemplateReadResponse {
    pub template: EnvironmentTemplateView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCredentialView {
    pub environment_id: EnvironmentId,
    pub env_name: String,
    pub source: EnvironmentCredentialSourceView,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EnvironmentCredentialSourceView {
    AuthGrant { grant_id: String },
    AuthProviderCredential { provider_id: String },
    DirectSecret { secret_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCredentialBindParams {
    pub environment_id: EnvironmentId,
    pub env_name: String,
    pub source: EnvironmentCredentialSourceView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCredentialBindResponse {
    pub credential: EnvironmentCredentialView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCredentialListParams {
    pub environment_id: EnvironmentId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCredentialListResponse {
    #[serde(default)]
    pub credentials: Vec<EnvironmentCredentialView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCredentialUnbindParams {
    pub environment_id: EnvironmentId,
    pub env_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCredentialUnbindResponse {
    pub credential: EnvironmentCredentialView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobHandleView {
    pub environment_id: EnvironmentId,
    pub job_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobHandleInput {
    pub environment_id: EnvironmentId,
    pub job_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobStartSpecInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<SessionJobDependencyInput>,
    #[serde(default)]
    pub dependency_policy: SessionJobDependencyPolicyView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobDependencyInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionJobDependencyPolicyView {
    #[default]
    AllSucceeded,
    AllTerminal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobStartedView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub job_id: String,
    pub handle: SessionJobHandleView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promise_id: Option<String>,
    pub status: SessionJobStatusView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
}

/// One job's summary and output page, or why it could not be read: an
/// entry with `error` and no summary names a job the environment does not
/// know (never created, or garbage-collected).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobReadEntryView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<SessionJobHandleView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SessionJobSummaryView>,
    /// Output in offset order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_chunks: Vec<SessionJobOutputChunkView>,
    /// Byte offset to continue reading from: pass it as `after`.
    pub output_next: u64,
    /// Output bytes the job produced so far; final once it is terminal.
    pub output_end: u64,
    /// Output ranges between the requested start and `outputNext` that
    /// cannot be returned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_gaps: Vec<SessionJobOutputGapView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobCancelEntryView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<SessionJobHandleView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SessionJobSummaryView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionJobCancelScopeView {
    #[default]
    Job,
    Dependents,
}

/// What a job is and how it ended. Instants are the environment's wall
/// clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobSummaryView {
    pub job_id: String,
    /// The start request that created the job; part of its identity.
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub status: SessionJobStatusView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
    /// When a cancel reached the job while it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_requested_at_ms: Option<i64>,
    /// Run time from start to finish, immune to clock jumps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// How the job's root process ended, once it ran and exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<SessionJobExitView>,
    /// Set when Lightspeed stopped the running job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_by: Option<SessionJobStoppedByView>,
    /// True when the job's root process exited while descendants it spawned
    /// were still running; the host terminated them best-effort at job
    /// completion.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub orphaned_descendants: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
}

/// Job lifecycle. `queued` and `running` are live; the rest are terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionJobStatusView {
    /// Waiting for dependencies or its queue.
    Queued,
    Running,
    /// Exited with code 0.
    Succeeded,
    /// Exited otherwise, or could not start.
    Failed,
    /// Its timeout expired and it was stopped.
    TimedOut,
    /// Stopped or never started because of a cancel.
    Cancelled,
    /// A dependency did not finish as the dependency policy requires.
    DependencyFailed,
    /// The environment daemon stopped while the job was queued or running.
    Interrupted,
}

/// How a job's root process ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionJobExitView {
    /// Normal exit with this status code.
    Code { code: i32 },
    /// Killed by this signal, named like `SIGSEGV`.
    Signal {
        signal: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        core_dumped: bool,
    },
}

/// Why Lightspeed stopped a running job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionJobStoppedByView {
    /// Its timeout expired: terminate, a grace period, then kill.
    Timeout,
    /// A cancel without `force`: terminate, a grace period, then kill.
    Terminate,
    /// A cancel with `force`: killed at once.
    Kill,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobOutputChunkView {
    /// Byte offset of the chunk's first byte in the job's combined output.
    pub offset: u64,
    pub stream: SessionJobOutputStreamView,
    pub data_base64: String,
}

/// Output bytes in `[offset, offset + omittedBytes)` that cannot be
/// returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobOutputGapView {
    pub offset: u64,
    pub omitted_bytes: u64,
    pub reason: SessionJobOutputGapReasonView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionJobOutputGapReasonView {
    /// Dropped from the middle because the job's retained output exceeded
    /// its cap; the head and the newest bytes are kept.
    Overflow,
    /// Released after it was delivered.
    Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionJobOutputStreamView {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentJobCreateParams {
    pub environment_id: EnvironmentId,
    pub request_id: String,
    pub jobs: Vec<SessionJobStartSpecInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentJobCreateResponse {
    pub environment_id: EnvironmentId,
    pub job_group_id: EnvironmentJobGroupId,
    #[serde(default)]
    pub jobs: Vec<SessionJobStartedView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentJobReadParams {
    pub jobs: Vec<SessionJobHandleInput>,
    /// Most output bytes per job; omit for the server's default page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Byte offset to read each job's output from: a previous entry's
    /// `outputNext`. Omitted with `tailBytes` omitted reads from the start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// Read each job's last `tailBytes` of output instead; excludes `after`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentJobReadResponse {
    #[serde(default)]
    pub jobs: Vec<SessionJobReadEntryView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentJobCancelParams {
    pub jobs: Vec<SessionJobHandleInput>,
    #[serde(default)]
    pub scope: SessionJobCancelScopeView,
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentJobCancelResponse {
    #[serde(default)]
    pub jobs: Vec<SessionJobCancelEntryView>,
}
