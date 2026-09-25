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
    /// Connection to an envd instance reachable from Lightspeed.
    pub connection: EnvironmentConnectionView,
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
        connection: EnvironmentConnectionView,
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
pub struct EnvironmentConnectionView {
    pub endpoint: String,
    pub transport: EnvironmentConnectionTransportView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentConnectionTransportView {
    WebSocket,
    Http,
    Stdio,
    Ssh,
    Provider { provider_type: String },
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
    pub public_ingress_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Registered environments only: when the gateway last saw the daemon's
    /// control connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobReadEntryView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<SessionJobHandleView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SessionJobSummaryView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_chunks: Vec<SessionJobOutputChunkView>,
    pub output_next_seq: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<SessionJobArtifactView>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobSummaryView {
    pub namespace: String,
    pub job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub status: SessionJobStatusView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionJobStatusView {
    Accepted,
    Queued,
    Running,
    Succeeded,
    Failed,
    CancelRequested,
    Cancelled,
    TimedOut,
    DependencyFailed,
    Interrupted,
    Lost,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobOutputChunkView {
    pub seq: u64,
    pub stream: SessionJobOutputStreamView,
    pub data_base64: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionJobOutputStreamView {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobArtifactView {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
    #[serde(default)]
    pub include_artifacts: bool,
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
