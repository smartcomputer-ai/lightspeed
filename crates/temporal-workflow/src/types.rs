use std::collections::BTreeMap;

use engine::{
    BlobRef, CommandRejection, ContextEntryInput, CoreAgentCommand, CoreAgentState,
    EmissionEnvelope, ManagedSessionWorkflowTools, RunStatus, SessionConfig, SessionId,
    SessionPosition, SubmissionId, ToolBatchId,
    storage::{SessionRecord, UncommittedStoredEvent},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionArgs {
    /// Universe (tenant) that owns this session. Activities route storage and
    /// runtime resources by the universe embedded in the workflow id, which
    /// bootstrap asserts equals `compose_workflow_id(universe_id, session_id)`.
    pub universe_id: Uuid,
    pub session_id: SessionId,
    /// Human-readable session name persisted as store metadata at creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Descriptive key/value metadata persisted on the session row at
    /// creation (validated at the API boundary); ignored when the row exists.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Root-only automatic deletion duration, applied only when the session
    /// row is first created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_after_close_ms: Option<u64>,
    pub session_config: SessionConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<crate::SessionProfileIntent>,
    /// Present only for the trusted managed-session creation path. The
    /// declaration is validated against `universe_id` and recorded as an
    /// immutable creation fact on the first append.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_tools: Option<ManagedSessionWorkflowTools>,
    /// Legacy cutover field accepted from workflows started before active-run
    /// rollover support.
    /// Hosted drive ignores it, and continuation/new-session payloads never
    /// serialize it.
    #[serde(default, rename = "max_steps_per_input", skip_serializing)]
    pub legacy_max_steps_per_input: Option<u32>,
    pub continue_as_new_history_threshold: Option<u32>,
    #[serde(default)]
    pub close_on_terminal: bool,
    /// One-shot unattended child sessions cannot expose approval UI. They
    /// append explicit rejection continuations instead of parking forever.
    #[serde(default)]
    pub auto_reject_approvals: bool,
    /// Workflow-local query state that is not reconstructible from the
    /// PostgreSQL session log. Transport queues are deliberately drained
    /// instead of being copied into this payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_state: Option<AgentSessionContinuationState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionContinuationState {
    pub version: u32,
    #[serde(default = "legacy_continuation_ready")]
    pub ready: bool,
    #[serde(default)]
    pub operation_outcomes: crate::SessionOperationReceipts,
    #[serde(default)]
    pub admission_failures: Vec<AgentAdmissionFailure>,
}

// Continuations written before workflow-owned setup already passed gateway
// setup. Preserve their ability to resume active runs after a worker upgrade.
fn legacy_continuation_ready() -> bool {
    true
}

impl AgentSessionContinuationState {
    pub const VERSION: u32 = 1;

    pub fn v1(admission_failures: Vec<AgentAdmissionFailure>) -> Self {
        Self {
            version: Self::VERSION,
            ready: false,
            operation_outcomes: crate::SessionOperationReceipts::default(),
            admission_failures,
        }
    }
}

/// Compose the Temporal workflow id for a session:
/// `{universe_id}/{session_id}`.
///
/// All universes of a deployment share one task queue and one Temporal
/// namespace; the universe prefix is what keeps client-chosen session ids
/// collision-free across universes. `/` is reserved as the separator — session
/// ids reject it (`api::validate_session_id`) and universe ids are UUIDs, so
/// the composed id splits unambiguously.
pub fn compose_workflow_id(universe_id: Uuid, session_id: &SessionId) -> String {
    format!("{universe_id}/{session_id}")
}

pub fn compose_environment_job_workflow_id(
    universe_id: Uuid,
    environment_id: &str,
    job_group_id: &str,
) -> String {
    format!("{universe_id}/envjob-{environment_id}-{job_group_id}")
}

/// Split a composed workflow id back into `(universe_id, session_id)`.
/// Returns `None` for ids that do not match the composed format, including a
/// session part that is not a valid session id.
pub fn split_workflow_id(workflow_id: &str) -> Option<(Uuid, SessionId)> {
    let (universe, session) = workflow_id.split_once('/')?;
    let universe_id = Uuid::parse_str(universe).ok()?;
    let session_id = SessionId::try_new(session).ok()?;
    Some((universe_id, session_id))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAdmission {
    pub command: CoreAgentCommand,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionStatus {
    pub session_id: String,
    pub initialized: bool,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub setup_error: Option<api::AgentApiError>,
    pub pending_admissions: usize,
    #[serde(default)]
    pub pending_tool_batch_resumes: usize,
    #[serde(default)]
    pub active_waits: usize,
    #[serde(default)]
    pub pending_emissions: usize,
    pub active_run: Option<AgentActiveRunSummary>,
    pub queued_runs: Vec<AgentQueuedRunSummary>,
    pub completed_runs: Vec<AgentCompletedRunSummary>,
    #[serde(default)]
    pub admission_failures: Vec<AgentAdmissionFailure>,
    pub last_error: Option<String>,
    /// True when the session workflow failed during bootstrap/rehydration. The
    /// gateway surfaces this as a typed `session_bootstrap_failed` error.
    #[serde(default)]
    pub bootstrap_failed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAdmissionFailure {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation_error: Option<api::AgentApiError>,
    pub submission_id: Option<SubmissionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_token: Option<String>,
    pub kind: AgentAdmissionFailureKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection: Option<CommandRejection>,
}

impl AgentAdmissionFailure {
    pub fn with_correlation_token(mut self, correlation_token: Option<String>) -> Self {
        if self.correlation_token.is_none() {
            self.correlation_token = correlation_token;
        }
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAdmissionFailureKind {
    RejectedCommand,
    UnsupportedAudioMime,
    AudioBlobMissing,
    AudioBlobTooLarge,
    AudioDurationTooLong,
    TranscoderUnavailable,
    TranscodeFailure,
    TranscriptionFailure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActiveRunSummary {
    pub run_id: u64,
    pub status: RunStatus,
    pub submission_id: Option<SubmissionId>,
    pub output: Option<engine::ContentRef>,
    pub active_turn_id: Option<u64>,
    pub active_tool_batch_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentQueuedRunSummary {
    pub run_id: u64,
    pub submission_id: Option<SubmissionId>,
    pub input: Vec<ContextEntryInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCompletedRunSummary {
    pub run_id: u64,
    pub status: RunStatus,
    pub submission_id: Option<SubmissionId>,
    pub output: Option<engine::ContentRef>,
    pub failure_message_ref: Option<BlobRef>,
}

/// Queued outbound emission on the producing side. Transient transport
/// state: the flush queue gates continue-as-new instead of being carried
/// through it, so delivery is at-least-once with idempotent receive keyed by
/// the emission id or the receiver's semantic token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEmission {
    pub receiver_workflow_id: String,
    pub envelope: EmissionEnvelope,
    /// Delivery attempts so far. Pushed workflow-tool invocation envelopes
    /// retry independently; other bodies keep the legacy single-attempt,
    /// drop-on-missing semantics.
    #[serde(default)]
    pub attempts: u32,
    /// Earliest workflow time this entry may be (re)sent; `0` is immediate.
    #[serde(default)]
    pub next_attempt_at_ms: u64,
}

impl PendingEmission {
    pub fn immediate(receiver_workflow_id: String, envelope: EmissionEnvelope) -> Self {
        Self {
            receiver_workflow_id,
            envelope,
            attempts: 0,
            next_attempt_at_ms: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPromiseCancellation {
    pub promise_id: String,
    pub source: engine::PromiseSource,
    /// Log sequence of the cancellation event, used as the emission
    /// producer sequence for per-key workflow-tool cancellation facts.
    #[serde(default)]
    pub log_seq: u64,
}

/// One received source-resolution awaiting producer authorization and
/// optional reply-schema validation before it becomes a `ResolvePromise`
/// admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSourceResolution {
    pub promise_id: engine::PromiseId,
    pub resolution: engine::PromiseResolution,
    pub producer: engine::EmissionProducer,
}

/// Fixed, versioned recovery query every start-on-call plugin workflow must
/// expose: the bounded per-key terminal resolutions it has produced so far.
/// The holder-side monitor uses it to distinguish a valid result whose
/// terminal signal was not observed, workflow failure/cancellation, and the
/// contract violation of completing while keyed promises remain pending.
pub const WORKFLOW_TOOL_RECOVERY_QUERY: &str = "workflow_tool_recovery";

/// Result shape of [`WORKFLOW_TOOL_RECOVERY_QUERY`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowToolRecoveryResult {
    #[serde(default)]
    pub resolutions: BTreeMap<String, engine::PromiseResolution>,
}

/// Start arguments every start-on-call plugin workflow receives. The plugin
/// resolves each keyed completion promise by emitting `SourceResolution`
/// bodies to the holder workflow through the fixed `deliver_emission`
/// signal, using its own execution id as the producer workflow id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowToolStartArgs {
    pub universe_id: uuid::Uuid,
    pub holder_workflow_id: String,
    pub execution_id: String,
    pub invocation: engine::WorkflowToolInvocation,
}

/// Prefix of every start-on-call recipe fingerprint.
pub const WORKFLOW_TOOL_RECIPE_FINGERPRINT_PREFIX: &str = "wtr:sha256:";

/// Canonical fingerprint over the raw recipe bytes; trusted managed-session
/// creators compute it when declaring a start binding and the start
/// activity re-verifies it before resolving the recipe.
pub fn workflow_tool_recipe_fingerprint(recipe_bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(recipe_bytes);
    format!(
        "{WORKFLOW_TOOL_RECIPE_FINGERPRINT_PREFIX}{}",
        hex::encode(hasher.finalize())
    )
}

/// Recipe format 1: a JSON object naming the plugin workflow type and task
/// queue. `recipe_format` identifies this codec, never a feature or plugin.
pub const WORKFLOW_TOOL_RECIPE_FORMAT_V1: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowToolRecipeV1 {
    pub workflow_type: String,
    pub task_queue: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolStartActivityRequest {
    pub execution_id: String,
    pub recipe_format: u32,
    pub recipe_revision: u32,
    pub recipe_ref: engine::BlobRef,
    pub recipe_fingerprint: String,
    pub holder_workflow_id: String,
    pub invocation: engine::WorkflowToolInvocation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum WorkflowToolStartActivityResult {
    /// The deterministic execution is running or already existed
    /// (`AlreadyStarted` is success for the exact identity).
    Started,
    /// The start cannot succeed without operator intervention (bad recipe,
    /// fingerprint mismatch, unknown format).
    FailedTerminal { message: String },
    /// Transient failure; the workflow retries with bounded backoff.
    FailedRetryable { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolExecutionCheckRequest {
    pub execution_id: String,
    pub completion_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolExecutionCancelRequest {
    pub execution_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolReplyValidationRequest {
    pub reply_schema_ref: engine::BlobRef,
    pub payload_ref: Option<engine::BlobRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum WorkflowToolReplyValidationResult {
    Valid,
    Invalid { error_ref: engine::BlobRef },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseSourcePoll {
    pub promise_id: String,
    pub source: engine::PromiseSource,
    pub next_check_at_ms: u64,
    pub poll_attempt: u32,
}

/// Ref-only snapshot of the promises joined calls completed on. The activity
/// prepares, per promise, the context entries its payload supplies beside
/// the call result (media a child handed up, for example).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinedContextPreparationRequest {
    #[serde(default)]
    pub results: Vec<AwaitPromiseResult>,
}

/// The combined await result plus the context entries the awaited payloads
/// supply beside it, bounded as one result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitMaterializationResult {
    pub result_ref: BlobRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_context: Vec<engine::ContextEntryInput>,
}

/// Bounded, ref-only snapshot passed to the storage-backed await materializer.
/// The activity replaces root refs with their JSON/text values without moving
/// the root bytes through workflow history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitMaterializationRequest {
    pub outcome: AwaitOutcome,
    #[serde(default)]
    pub results: Vec<AwaitPromiseResult>,
}

/// Canonical model-visible value written by the await materializer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedAwaitResult {
    pub outcome: AwaitOutcome,
    #[serde(default)]
    pub results: Vec<MaterializedAwaitPromiseResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedAwaitPromiseResult {
    pub promise_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitOutcome {
    Terminal,
    Timeout,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitPromiseResult {
    pub promise_id: String,
    /// `pending | resolved | failed | cancelled`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_ref: Option<BlobRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_ref: Option<BlobRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingToolBatchResume {
    pub batch_id: ToolBatchId,
    pub command: engine::ResumeToolBatchCommand,
}

/// Armed while the active run sits in `cancelling`; the workflow forces the
/// run terminal once the deadline passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancellingWatchdog {
    pub run_id: u64,
    pub since_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobStartActivityRequest {
    pub universe_id: Uuid,
    pub environment_id: String,
    pub job_group_id: String,
    pub request_ref: BlobRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobStartPayload {
    pub request: environment_protocol::data::jobs::StartJobsParams,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobStartActivityResult {
    pub jobs: Vec<environment_protocol::data::jobs::JobSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobSubscription {
    pub holder_workflow_id: String,
    pub promise_id: String,
    pub completion_key: String,
    pub job_id: environment_protocol::shared::JobId,
    #[serde(default)]
    pub notified: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobWorkflowArgs {
    pub universe_id: Uuid,
    pub start: EnvironmentJobStartActivityRequest,
    pub job_ids: Vec<environment_protocol::shared::JobId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<EnvironmentJobSubscription>,
    #[serde(default)]
    pub started: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<environment_protocol::data::jobs::JobSummary>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolutions: BTreeMap<String, engine::PromiseSourceCheckResult>,
    #[serde(default = "default_environment_job_poll_ms")]
    pub poll_ms: u64,
    #[serde(default)]
    pub poll_attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_tool: Option<EnvironmentJobWorkflowToolContext>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobWorkflowToolContext {
    pub execution_id: String,
    pub invocation_id: engine::WorkflowToolInvocationId,
}

/// Accepted inputs for the core job workflow. Bare public starts arrive as
/// fully prepared job arguments; internally supervised starts use the fixed
/// internally supervised input and are normalized by an activity before
/// provider work begins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnvironmentJobWorkflowInput {
    WorkflowTool(WorkflowToolStartArgs),
    Job(EnvironmentJobWorkflowArgs),
}

impl From<EnvironmentJobWorkflowArgs> for EnvironmentJobWorkflowInput {
    fn from(value: EnvironmentJobWorkflowArgs) -> Self {
        Self::Job(value)
    }
}

/// Sub-agent execution: one delegation supervised by
/// `SubagentExecutionWorkflow`, started on call by the parent session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentPrepareActivityRequest {
    pub start: WorkflowToolStartArgs,
}

/// Outcome of preparing a delegation. `Rejected` is a terminal, expected
/// failure (limit exceeded, unlisted agent, missing profile) that resolves
/// the parent's `reply` promise as failed without a child ever existing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SubagentPrepareActivityResult {
    Prepared {
        child: SubagentChildRef,
        /// Grant deadline for the child's run, in milliseconds.
        deadline_ms: u64,
    },
    Rejected {
        error_ref: BlobRef,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentChildRef {
    pub session_id: String,
    pub run_id: u64,
    pub agent_profile_id: String,
}

/// How the child's run ended, as observed by the execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SubagentTerminal {
    Run {
        status: engine::RunStatus,
        output: Option<engine::ContentRef>,
        failure_message_ref: Option<BlobRef>,
    },
    Deadline,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentResolveActivityRequest {
    pub universe_id: Uuid,
    pub child: SubagentChildRef,
    pub terminal: SubagentTerminal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentCloseActivityRequest {
    pub universe_id: Uuid,
    pub session_id: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentExecutionPhase {
    #[default]
    Starting,
    Preparing,
    Running,
    Resolved,
    Cancelled,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentExecutionSnapshot {
    pub phase: SubagentExecutionPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<SubagentChildRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<engine::PromiseResolution>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobPrepareWorkflowToolRequest {
    pub start: WorkflowToolStartArgs,
}

fn default_environment_job_poll_ms() -> u64 {
    2_000
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobWorkflowSnapshot {
    pub environment_id: String,
    pub job_group_id: String,
    #[serde(default)]
    pub started: bool,
    #[serde(default)]
    pub jobs: Vec<environment_protocol::data::jobs::JobSummary>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolutions: BTreeMap<String, engine::PromiseSourceCheckResult>,
    pub terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobPollActivityRequest {
    pub universe_id: Uuid,
    pub environment_id: String,
    pub job_group_id: String,
    pub job_ids: Vec<environment_protocol::shared::JobId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobPollActivityResult {
    pub jobs: Vec<environment_protocol::data::jobs::JobSummary>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolutions: BTreeMap<String, engine::PromiseSourceCheckResult>,
    pub terminal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobCancelSignal {
    pub jobs: Vec<environment_protocol::shared::JobId>,
    pub scope: environment_protocol::data::jobs::JobCancelScope,
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobCancelActivityRequest {
    pub universe_id: Uuid,
    pub environment_id: String,
    pub jobs: Vec<environment_protocol::shared::JobId>,
    pub scope: environment_protocol::data::jobs::JobCancelScope,
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOrLoadSessionRequest {
    pub session_id: SessionId,
    /// Applied only when the session is created; ignored on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Applied only when the session is created; ignored on load.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Applied only when the session is created; ignored on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_after_close_ms: Option<u64>,
    pub observed_at_ms: u64,
}

/// Compact session rehydration result.
///
/// The bootstrap activity reduces the durable session log internally and returns
/// only the replayed `CoreAgentState` plus the small workflow-only indices it
/// reconstructs. The full event log is never transported through the activity
/// result (and therefore never recorded in Temporal history), which is what
/// previously failed long-lived sessions with `Complete result exceeds size
/// limit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOrLoadSessionResult {
    pub record: SessionRecord,
    /// Replayed reduced agent state. `None` for a freshly created session with
    /// no persisted events yet (the workflow then opens a new session).
    pub core_state: Option<CoreAgentState>,
    /// `run_id` -> originating submission id, reconstructed from accepted-run
    /// events. Empty for a fresh session.
    #[serde(default)]
    pub run_submissions: BTreeMap<u64, Option<SubmissionId>>,
    /// Current durable log head after replay.
    pub head: Option<SessionPosition>,
    /// True only when the authoritative durable head was empty at bootstrap.
    /// This must not be inferred from tail replay counts because a checkpoint
    /// can make a non-empty session replay zero events.
    pub fresh_session: bool,
    /// Number of persisted tail events replayed during this bootstrap.
    pub replayed_event_count: u64,
}

/// Typed bootstrap failure surfaced when the compact rehydration result would
/// still exceed the configured Temporal payload budget, so the failure is
/// diagnosable instead of an opaque `Complete result exceeds size limit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBootstrapPayloadTooLarge {
    pub session_id: SessionId,
    pub reduced_state_bytes: u64,
    pub budget_bytes: u64,
    pub replayed_event_count: u64,
}

impl std::fmt::Display for SessionBootstrapPayloadTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "session bootstrap payload too large: session_id={} \
             reduced_state_bytes={} budget_bytes={} replayed_event_count={}",
            self.session_id, self.reduced_state_bytes, self.budget_bytes, self.replayed_event_count,
        )
    }
}

impl std::error::Error for SessionBootstrapPayloadTooLarge {}

/// Application-failure type name for a transient LLM provider error.
/// The worker attaches it to a retryable `ApplicationFailure`; the session
/// workflow recognizes it in an exhausted activity failure's cause chain and
/// converts it into a terminal failed generation/compaction result.
pub const LLM_PROVIDER_TRANSIENT_ERROR_TYPE: &str = "llm_provider_transient";

/// Compact versioned detail payload carried by the `llm_provider_transient`
/// application failure. Session, run, turn, provider, and request identity
/// are not duplicated here — the workflow retains the original request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmTransientFailureDetails {
    pub version: u32,
    pub message: String,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

pub const LLM_TRANSIENT_FAILURE_DETAILS_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutBlobRequest {
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadBlobRequest {
    pub blob_ref: BlobRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadBlobResult {
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendEventsRequest {
    pub session_id: SessionId,
    pub expected_head: Option<SessionPosition>,
    pub events: Vec<UncommittedStoredEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmGenerateActivityRequest {
    pub request: engine::LlmGenerationRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreprocessRunInputActivityRequest {
    pub session_id: SessionId,
    pub input: Vec<ContextEntryInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreprocessRunInputActivityResult {
    pub outcome: PreprocessRunInputOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum PreprocessRunInputOutcome {
    Succeeded { input: Vec<ContextEntryInput> },
    Failed { failure: PreprocessRunInputFailure },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreprocessRunInputFailure {
    pub kind: PreprocessRunInputFailureKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreprocessRunInputFailureKind {
    UnsupportedAudioMime,
    AudioBlobMissing,
    AudioBlobTooLarge,
    AudioDurationTooLong,
    TranscoderUnavailable,
    TranscodeFailure,
    TranscriptionFailure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompactActivityRequest {
    pub request: engine::ContextCompactionRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvokeBatchActivityRequest {
    pub request: engine::ToolInvocationBatchRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvokeCallActivityRequest {
    pub request: engine::ToolInvocationCallRequest,
}

/// Outcome of one per-call tool activity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolInvokeCallActivityResult {
    /// The call ran (or failed at tool level) and produced a terminal result.
    Completed {
        result: engine::ToolInvocationResult,
    },
    /// The call did not execute: the session's active environment is still
    /// provisioning or booting. The workflow waits for readiness with
    /// `await_environment_ready` and re-dispatches the same call.
    EnvironmentNotReady { environment_id: String },
    /// The native MCP call must receive a single-use run-owned decision
    /// before the worker performs any MCP wire I/O.
    NeedsApproval { subject: engine::ApprovalSubject },
}

/// Wait for the session's active environment to become reachable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitEnvironmentReadyActivityRequest {
    pub session_id: SessionId,
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_policy: Option<engine::EnvironmentPolicyRuntime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AwaitEnvironmentReadyActivityResult {
    Ready,
    /// The environment reached a terminal non-ready state (failed, closed) or
    /// became unusable for this session.
    Failed {
        message: String,
    },
    /// The bounded readiness window elapsed while the environment was still
    /// provisioning or booting.
    TimedOut {
        last_status: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPreparePromiseControlsActivityRequest {
    pub request: engine::PromiseControlArgumentRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeProjectionRefreshActivityRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<engine::EnvironmentsFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_environment_id: Option<engine::EnvironmentId>,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_links: Vec<engine::WorkspaceLink>,
    pub vfs_catalog_enabled: bool,
    pub vfs_prompts_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfs_prompt_roots: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub active_instruction_inputs: BTreeMap<engine::ContextEntryKey, engine::ContextEntryInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfs_skills: Option<engine::VfsSkillsConfig>,
    /// Current keyed catalogs, including source provenance, for publication comparison.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub active_catalogs: BTreeMap<engine::ContextEntryKey, engine::ContextEntryInput>,
    /// The admitted sub-agent grant; the catalog entry follows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<engine::SubagentsFeature>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeProjectionRefreshActivityResult {
    pub commands: Vec<CoreAgentCommand>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_id_composition_round_trips() {
        let universe_id = Uuid::parse_str("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f").expect("uuid");
        let session_id = SessionId::new("session_mybot");
        let workflow_id = compose_workflow_id(universe_id, &session_id);
        assert_eq!(
            workflow_id,
            "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f/session_mybot"
        );
        let (split_universe, split_session) =
            split_workflow_id(&workflow_id).expect("split composed id");
        assert_eq!(split_universe, universe_id);
        assert_eq!(split_session, session_id);
    }

    #[test]
    fn split_workflow_id_rejects_non_composed_ids() {
        // Legacy ids were the bare session id; they must not silently parse.
        assert_eq!(split_workflow_id("session_mybot"), None);
        assert_eq!(split_workflow_id("not-a-uuid/session_mybot"), None);
        assert_eq!(
            split_workflow_id("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f/"),
            None
        );
        assert_eq!(split_workflow_id(""), None);
    }

    #[test]
    fn split_workflow_id_rejects_extra_separators() {
        // Session ids reject '/', so the first separator is authoritative and
        // a second one makes the session part invalid.
        let universe_id = Uuid::parse_str("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f").expect("uuid");
        assert_eq!(split_workflow_id(&format!("{universe_id}/a/b")), None);
    }
}
