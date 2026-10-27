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
    pub request_id: EnvironmentProvisionRequestId,
    pub binding_id: EnvironmentProviderBindingId,
    /// Immutable provider template-version identity.
    pub template_id: EnvironmentTemplateId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Optional staged idle policy applied by the power reaper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_policy: Option<EnvironmentIdlePolicyView>,
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<EnvironmentProviderId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<EnvironmentProviderBindingId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<EnvironmentLifecycleStatusView>,
    /// Only registered environments admitted by this registration key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_key_id: Option<EnvironmentRegistrationKeyId>,
    /// Only environments carrying every listed metadata pair (AND
    /// semantics); the same filter `session/list` accepts.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentListResponse {
    #[serde(default)]
    pub environments: Vec<EnvironmentView>,
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
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentIngressPutResponse {
    pub environment: EnvironmentView,
}

/// Steady power state of a provisioned environment.
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

/// Staged idle policy. Thresholds are milliseconds of daemon-reported idle
/// time and must be non-decreasing in the order pause, suspend, stop, close.
/// Stages whose power state the provider does not support are skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
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
    /// Desired steady power state. Must be one of the provider-reported
    /// `incarnation.powerStates`.
    pub power: EnvironmentPowerStateView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentPowerPutResponse {
    pub environment: EnvironmentView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentIdlePolicyPutParams {
    pub environment_id: EnvironmentId,
    /// The complete new policy; omit to clear it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_policy: Option<EnvironmentIdlePolicyView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentIdlePolicyPutResponse {
    pub environment: EnvironmentView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentExternalCreateParams {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentLifecycleStatusView {
    Provisioning,
    Booting,
    Ready,
    /// Execution frozen; wakes on next use.
    Paused,
    /// Execution state saved to disk; wakes on next use.
    Suspended,
    /// Powered off; provisioned environments wake on next use when the
    /// provider supports power control.
    Offline,
    Closing,
    Closed,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EnvironmentSourceView {
    Provisioned {
        provider_id: EnvironmentProviderId,
        binding_id: EnvironmentProviderBindingId,
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
        identity_mode: EnvironmentIdentityModeView,
    },
}

/// What Lightspeed does with a registered environment while its daemon is
/// disconnected. Registration-key policy, copied onto each environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentIdentityModeView {
    /// Stays offline until explicitly closed.
    Persistent,
    /// Closed once the daemon has been away longer than the key's
    /// disconnect grace.
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
    /// First characters of the secret, for identification only.
    pub key_prefix: String,
    pub identity_mode: EnvironmentIdentityModeView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active_environments: Option<u32>,
    /// Effective disconnect grace for ephemeral environments.
    pub ephemeral_disconnect_grace_ms: u64,
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
    pub identity_mode: EnvironmentIdentityModeView,
    /// Non-closed environments the key may have at once; omit for unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active_environments: Option<u32>,
    /// Disconnect grace for ephemeral environments; omit for the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_disconnect_grace_ms: Option<u64>,
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
pub struct EnvironmentIncarnationView {
    pub incarnation_id: EnvironmentIncarnationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provision_request_id: Option<EnvironmentProvisionRequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_target_id: Option<EnvironmentTargetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<EnvironmentTemplateId>,
    /// Power states the provider reported for this target; empty until
    /// observed or when the provider offers no power control.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub power_states: Vec<EnvironmentPowerStateView>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentView {
    pub environment_id: EnvironmentId,
    pub request_id: EnvironmentProvisionRequestId,
    pub source: EnvironmentSourceView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub status: EnvironmentLifecycleStatusView,
    /// Lightspeed-owned power intent; `status` is the observed state.
    pub desired_power: EnvironmentPowerStateView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_policy: Option<EnvironmentIdlePolicyView>,
    pub incarnation: EnvironmentIncarnationView,
    /// Lightspeed-owned public ingress intent; `publicEndpoint` is what the
    /// provider realized.
    pub public_ingress_enabled: bool,
    /// The public endpoint the provider last reported. It lags
    /// `publicIngressEnabled` while the lifecycle reconciler converges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Registered environments only: when the gateway last saw the daemon's
    /// control connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at_ms: Option<i64>,
    /// Why the environment failed, or the latest error (or what the
    /// provider reports it is waiting on) while its lifecycle converges;
    /// absent once it reaches a steady state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_error: Option<EnvironmentLifecycleErrorView>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentLifecycleErrorView {
    pub message: String,
    /// When the current run of errors began.
    pub first_at_ms: i64,
    /// When `message` was recorded.
    pub last_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentProviderBindingStatusView {
    Enabled,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderBindingView {
    pub binding_id: EnvironmentProviderBindingId,
    pub provider_id: EnvironmentProviderId,
    pub status: EnvironmentProviderBindingStatusView,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
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
    pub binding_id: EnvironmentProviderBindingId,
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
    pub binding_id: EnvironmentProviderBindingId,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub public_ingress: bool,
    pub deprecated: bool,
    /// Compute targets of this template get; absent when the provider does
    /// not fix it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<EnvironmentTemplateResourcesView>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<EnvironmentProviderBindingId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTemplateListResponse {
    pub templates: Vec<EnvironmentTemplateView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTemplateReadParams {
    pub binding_id: EnvironmentProviderBindingId,
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
