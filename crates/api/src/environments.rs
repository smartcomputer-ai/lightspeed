use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentView {
    pub env_id: EnvironmentId,
    pub kind: SessionEnvironmentKindView,
    pub status: SessionEnvironmentStatusView,
    pub capabilities: SessionEnvironmentCapabilitiesView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_target: Option<ToolExecutionTargetView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionEnvironmentKindView {
    Sandbox,
    AttachedHost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionEnvironmentStatusView {
    Attaching,
    Ready,
    Degraded,
    Detached,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentCapabilitiesView {
    pub fs_read: bool,
    pub fs_write: bool,
    pub process_exec: bool,
    pub process_stdin: bool,
    #[serde(default)]
    pub job_start: bool,
    #[serde(default)]
    pub job_list: bool,
    #[serde(default)]
    pub job_read: bool,
    #[serde(default)]
    pub job_cancel: bool,
    #[serde(default)]
    pub job_wait_hint: bool,
    #[serde(default)]
    pub job_dependencies: bool,
    #[serde(default)]
    pub job_queue_keys: bool,
    pub network: bool,
    pub persistent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentListParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentListResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env_id: Option<EnvironmentId>,
    #[serde(default)]
    pub environments: Vec<SessionEnvironmentView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentReadParams {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentReadResponse {
    pub environment: SessionEnvironmentView,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentCreateParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<EnvironmentId>,
    pub provider_id: EnvironmentProviderId,
    pub request: HostTargetCreateRequestView,
    #[serde(default)]
    pub activate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentCreateResponse {
    pub environment: SessionEnvironmentView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env_id: Option<EnvironmentId>,
    #[serde(default)]
    pub environments: Vec<SessionEnvironmentView>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentAttachParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<EnvironmentId>,
    pub provider_id: EnvironmentProviderId,
    pub request: HostTargetAttachRequestView,
    #[serde(default)]
    pub activate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentAttachResponse {
    pub environment: SessionEnvironmentView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env_id: Option<EnvironmentId>,
    #[serde(default)]
    pub environments: Vec<SessionEnvironmentView>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostTargetCreateRequestView {
    Sandbox { spec: SandboxTargetSpecView },
    AttachedHost { spec: AttachedHostSpecView },
    Provider { provider_type: String, spec: Value },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SandboxTargetSpecView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttachedHostSpecView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostTargetAttachRequestView {
    Target { target_id: EnvironmentTargetId },
    Provider { provider_type: String, spec: Value },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentActivateParams {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentActivateResponse {
    pub environment: SessionEnvironmentView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env_id: Option<EnvironmentId>,
    #[serde(default)]
    pub environments: Vec<SessionEnvironmentView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentDeactivateParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentDeactivateResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env_id: Option<EnvironmentId>,
    #[serde(default)]
    pub environments: Vec<SessionEnvironmentView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentCloseParams {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_target: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvironmentCloseResponse {
    pub environment: SessionEnvironmentView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_env_id: Option<EnvironmentId>,
    #[serde(default)]
    pub environments: Vec<SessionEnvironmentView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobHandleView {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
    pub job_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobHandleInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<EnvironmentId>,
    pub job_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobCreateParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<EnvironmentId>,
    pub request_id: String,
    pub jobs: Vec<SessionJobStartSpecInput>,
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
pub struct SessionJobCreateResponse {
    pub env_id: EnvironmentId,
    #[serde(default)]
    pub jobs: Vec<SessionJobStartedView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobStartedView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub job_id: String,
    pub handle: SessionJobHandleView,
    pub status: SessionJobStatusView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobListParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_id: Option<EnvironmentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobListResponse {
    #[serde(default)]
    pub jobs: Vec<SessionJobHandleRecordView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobHandleRecordView {
    pub handle: SessionJobHandleView,
    pub provider_id: EnvironmentProviderId,
    pub target_id: EnvironmentTargetId,
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_turn_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_tool_call_id: Option<String>,
    pub created_at_ms: i64,
    pub start_request_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobReadParams {
    pub session_id: SessionId,
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
pub struct SessionJobReadResponse {
    #[serde(default)]
    pub jobs: Vec<SessionJobReadEntryView>,
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
pub struct SessionJobCancelParams {
    pub session_id: SessionId,
    pub jobs: Vec<SessionJobHandleInput>,
    #[serde(default)]
    pub scope: SessionJobCancelScopeView,
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJobCancelResponse {
    #[serde(default)]
    pub jobs: Vec<SessionJobCancelEntryView>,
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
pub struct EnvironmentProviderView {
    pub provider_id: EnvironmentProviderId,
    pub provider_kind: EnvironmentProviderKindView,
    pub status: EnvironmentProviderStatusView,
    pub controller_connection: HostControllerConnectionView,
    pub capabilities: EnvironmentProviderCapabilitiesView,
    pub implementation: EnvironmentProviderImplementationView,
    pub last_seen_ms: i64,
    pub lease_expires_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentProviderKindView {
    Sandbox,
    Bridge,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentProviderStatusView {
    Registering,
    Online,
    Stale,
    Offline,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostControllerConnectionView {
    pub endpoint: String,
    pub transport: HostTransportView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostTransportView {
    WebSocket,
    Http,
    Stdio,
    Ssh,
    Provider { provider_type: String },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderCapabilitiesView {
    #[serde(default)]
    pub list_targets: bool,
    #[serde(default)]
    pub create_target: bool,
    #[serde(default)]
    pub attach_target: bool,
    #[serde(default)]
    pub get_target: bool,
    #[serde(default)]
    pub close_target: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderImplementationView {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTargetSummaryView {
    pub target_id: EnvironmentTargetId,
    pub status: EnvironmentTargetStatusView,
    pub scope: HostScopeView,
    pub capabilities: HostCapabilitiesView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentTargetStatusView {
    Creating,
    Starting,
    Ready,
    Stopped,
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
pub enum HostScopeView {
    Default,
    Session { session_id: String },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostCapabilitiesView {
    #[serde(default)]
    pub filesystem_read: bool,
    #[serde(default)]
    pub filesystem_write: bool,
    #[serde(default)]
    pub process_start: bool,
    #[serde(default)]
    pub process_stdin: bool,
    #[serde(default)]
    pub process_terminate: bool,
    #[serde(default)]
    pub process_output_polling: bool,
    #[serde(default)]
    pub process_output_notifications: bool,
    #[serde(default)]
    pub process_pty: bool,
    #[serde(default)]
    pub job_start: bool,
    #[serde(default)]
    pub job_list: bool,
    #[serde(default)]
    pub job_read: bool,
    #[serde(default)]
    pub job_cancel: bool,
    #[serde(default)]
    pub job_wait_hint: bool,
    #[serde(default)]
    pub job_dependencies: bool,
    #[serde(default)]
    pub job_queue_keys: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderRegisterParams {
    pub provider_id: EnvironmentProviderId,
    pub provider_kind: EnvironmentProviderKindView,
    pub controller_connection: HostControllerConnectionView,
    pub capabilities: EnvironmentProviderCapabilitiesView,
    pub implementation: EnvironmentProviderImplementationView,
    pub lease_ttl_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderRegisterResponse {
    pub provider: EnvironmentProviderView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderHeartbeatParams {
    pub provider_id: EnvironmentProviderId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ttl_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed_targets: Vec<EnvironmentTargetSummaryView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderHeartbeatResponse {
    pub provider: EnvironmentProviderView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<EnvironmentTargetSummaryView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderUnregisterParams {
    pub provider_id: EnvironmentProviderId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderUnregisterResponse {
    pub provider: EnvironmentProviderView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<EnvironmentProviderStatusView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_kind: Option<EnvironmentProviderKindView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderListResponse {
    #[serde(default)]
    pub providers: Vec<EnvironmentProviderView>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderTargetListParams {
    pub provider_id: EnvironmentProviderId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<EnvironmentTargetStatusView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderTargetListResponse {
    #[serde(default)]
    pub targets: Vec<EnvironmentTargetSummaryView>,
}
