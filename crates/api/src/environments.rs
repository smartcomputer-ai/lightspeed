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
    pub incarnation: EnvironmentIncarnationView,
    pub public_ingress_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
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
