use std::collections::{BTreeMap, BTreeSet};

use harness::{
    BlobRef, CommandRejection, ContextEntryInput, CoreAgentCommand, CoreAgentState,
    DeclaredSessionWorkflowTools, EmissionEnvelope, RunStatus, SessionConfig, SessionId,
    SessionPosition, SubmissionId, ToolBatchId,
    storage::{SessionRecord, UncommittedStoredEvent},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Start arguments, continuation, bootstrap results and carries sit in Temporal history
/// and are decoded again on every replay. Decoding them is lenient, so a field added
/// later has a default and an older payload still loads. A container gets
/// `#[serde(default)]` when every field has an honest empty value. One holding an identity
/// or a session's model keeps that required and has a default on each other field, because
/// a made-up one would run real work under it.
/// A breaking release starts a new namespace and leaves old history behind, so renames
/// need no alias.
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
    /// Present only for the trusted declared-session creation path. The
    /// declaration is validated against `universe_id` and recorded as an
    /// immutable creation fact on the first append.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_tools: Option<DeclaredSessionWorkflowTools>,
    pub continue_as_new_history_threshold: Option<u32>,
    #[serde(default)]
    pub close_on_terminal: bool,
    /// One-shot unattended child sessions cannot expose approval UI. They
    /// append explicit rejection continuations instead of parking forever.
    #[serde(default)]
    pub auto_reject_approvals: bool,
    /// This execution takes over an open session whose last execution stopped. The
    /// log already holds the session's declaration and its setup, so neither is
    /// repeated or checked against these args. Rollovers keep the flag, because they
    /// carry the same args; only the first execution, which has no continuation state,
    /// recovers the interrupted work.
    #[serde(default)]
    pub restart: bool,
    /// Workflow-local query state that is not reconstructible from the
    /// PostgreSQL session log. Transport queues are deliberately drained
    /// instead of being copied into this payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_state: Option<AgentSessionContinuationState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSessionContinuationState {
    pub version: u32,
    pub ready: bool,
    pub operation_outcomes: crate::SessionOperationReceipts,
    #[serde(default)]
    pub admission_failures: Vec<AgentAdmissionFailure>,
    /// Started executions whose promises all went terminal and whose cancel has not
    /// gone out. The log says when a promise ended, not whether this was sent.
    #[serde(default)]
    pub pending_execution_cancels: BTreeSet<String>,
}

impl AgentSessionContinuationState {
    pub const VERSION: u32 = 1;
}

impl Default for AgentSessionContinuationState {
    fn default() -> Self {
        Self {
            version: Self::VERSION,
            ready: false,
            operation_outcomes: Default::default(),
            admission_failures: Vec::new(),
            pending_execution_cancels: BTreeSet::new(),
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
    pub ready: bool,
    #[serde(default)]
    pub setup_error: Option<api::AgentApiError>,
    pub pending_admissions: usize,
    /// Submissions among the pending admissions, including a run being prepared.
    /// Their outcome is still open, so a run matching one proves nothing yet.
    #[serde(default)]
    pub pending_submission_ids: Vec<SubmissionId>,
    #[serde(default)]
    pub pending_tool_batch_unparks: usize,
    #[serde(default)]
    pub active_waits: usize,
    #[serde(default)]
    pub pending_emissions: usize,
    pub active_run: Option<AgentActiveRunSummary>,
    pub queued_runs: Vec<AgentQueuedRunSummary>,
    pub finished_runs: Vec<AgentFinishedRunSummary>,
    #[serde(default)]
    pub admission_failures: Vec<AgentAdmissionFailure>,
    /// Why the execution is failing. Set only on paths that end the workflow,
    /// and the one status field waiters fail fast on.
    pub fatal_error: Option<String>,
    /// The latest advisory fact: an emission refused or a best-effort step that
    /// did not happen. The session carried on, so no gateway path reads this as
    /// an error. It stays until a newer warning replaces it or the execution
    /// rolls over, which starts the snapshot fresh.
    #[serde(default)]
    pub last_warning: Option<String>,
    /// True when the session workflow failed during bootstrap/rehydration. The
    /// gateway surfaces this as a typed `sessionBootstrapFailed` error.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAdmissionFailureKind {
    RejectedCommand,
}

impl AgentAdmissionFailure {
    /// What the caller of the refused command is told.
    pub fn api_error(&self) -> api::AgentApiError {
        if let Some(error) = &self.preparation_error {
            return error.clone();
        }
        match &self.rejection {
            Some(rejection) => command_rejection_to_api_error(rejection),
            None => api::AgentApiError::rejected(self.message.clone()),
        }
    }
}

/// The one classification of a harness refusal for API callers. Input the caller
/// must change is an invalid request, a lost race is a conflict, and a refusal by
/// current state stays `rejected`, which is what callers may retry.
pub fn command_rejection_to_api_error(rejection: &CommandRejection) -> api::AgentApiError {
    use harness::CommandRejectionKind as Kind;
    let kind = match rejection.kind {
        Kind::Invalid | Kind::ProviderCompatibility => api::AgentApiErrorKind::InvalidRequest,
        Kind::RevisionConflict | Kind::Conflict | Kind::ApprovalAlreadyTerminal => {
            api::AgentApiErrorKind::Conflict
        }
        Kind::Internal => api::AgentApiErrorKind::Internal,
        Kind::SessionLifecycle
        | Kind::ActiveWork
        | Kind::MissingActiveRun
        | Kind::UnknownReference
        | Kind::DuplicateSubmission
        | Kind::Refused => api::AgentApiErrorKind::Rejected,
    };
    api::AgentApiError::new(kind, rejection.message.clone())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActiveRunSummary {
    pub run_id: u64,
    pub status: RunStatus,
    pub submission_id: Option<SubmissionId>,
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
pub struct AgentFinishedRunSummary {
    pub run_id: u64,
    pub status: RunStatus,
    pub submission_id: Option<SubmissionId>,
    pub output: Option<harness::ContentRef>,
}

/// Queued outbound emission on the producing side. Transient transport
/// state: the flush queue gates continue-as-new instead of being carried
/// through it. Delivery reaches a running receiver and can repeat, so receipt
/// is idempotent, keyed by the emission id or the receiver's semantic token.
/// A receiver that is not running misses a run terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEmission {
    pub receiver_workflow_id: String,
    pub envelope: EmissionEnvelope,
    /// Delivery attempts so far. Pushed workflow-tool invocation envelopes
    /// retry independently; other bodies get one attempt and drop if the
    /// target is not running.
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
    pub source: harness::PromiseSource,
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
    pub promise_id: harness::PromiseId,
    pub resolution: harness::PromiseResolution,
    pub producer: harness::EmissionProducer,
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
    pub resolutions: BTreeMap<String, harness::PromiseResolution>,
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
    pub invocation: harness::WorkflowToolInvocation,
}

/// Prefix of every start-on-call recipe fingerprint.
pub const WORKFLOW_TOOL_RECIPE_FINGERPRINT_PREFIX: &str = "wtr:sha256:";

/// Canonical fingerprint over the raw recipe bytes; trusted declared-session
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
    pub recipe_ref: harness::BlobRef,
    pub recipe_fingerprint: String,
    pub holder_workflow_id: String,
    pub invocation: harness::WorkflowToolInvocation,
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
    pub resolution: harness::PromiseResolution,
    /// Checked against a `Resolved` payload, which it then requires.
    pub reply_schema_ref: Option<harness::BlobRef>,
    /// The reply becomes a call's result, which providers receive as text.
    pub text_reply: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum WorkflowToolReplyValidationResult {
    Valid,
    Invalid { error_ref: harness::BlobRef },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseSourcePoll {
    pub promise_id: String,
    pub source: harness::PromiseSource,
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
    pub additional_context: Vec<harness::ContextEntryInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<harness::Attachment>,
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

pub use harness::AwaitOutcome;

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
pub struct PendingToolBatchUnpark {
    pub batch_id: ToolBatchId,
    pub command: harness::UnparkToolBatchCommand,
}

/// Armed while the active run sits in `cancelling`; the workflow forces the
/// run terminal once the deadline passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancellingWatchdog {
    pub run_id: u64,
    pub since_ms: u64,
}

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
        /// Grant deadline for the child's run, in milliseconds. Absent when
        /// the run has no time limit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_timeout_ms: Option<u64>,
        /// How the execution supervises the wait.
        wait: SubagentWaitPolicy,
    },
    Rejected {
        error_ref: BlobRef,
    },
}

/// How an execution supervises its wait on the child, chosen by the worker and
/// recorded in history through the prepare result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentWaitPolicy {
    /// How often to ask whether the child's session workflow has stopped.
    pub liveness_check_interval_ms: u64,
    /// Each check is history, so a long wait rolls over at this length.
    pub continue_as_new_history_threshold: u32,
}

impl Default for SubagentWaitPolicy {
    fn default() -> Self {
        Self {
            liveness_check_interval_ms: u64::try_from(
                crate::SUBAGENT_LIVENESS_CHECK_INTERVAL.as_millis(),
            )
            .expect("static duration fits in u64 milliseconds"),
            continue_as_new_history_threshold: crate::DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD,
        }
    }
}

/// What `SubagentExecutionWorkflow` runs from: the start-on-call arguments, or its
/// own wait carried across a history rollover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SubagentExecutionInput {
    Start(WorkflowToolStartArgs),
    Resume(SubagentExecutionResume),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentExecutionResume {
    pub start: WorkflowToolStartArgs,
    pub child: SubagentChildRef,
    /// Workflow-clock instant the grant deadline falls on, fixed when the wait
    /// began so that rolling over does not move it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<u64>,
    pub wait: SubagentWaitPolicy,
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
        status: harness::RunStatus,
        output: Option<harness::ContentRef>,
        failure_message_ref: Option<BlobRef>,
    },
    Deadline,
    /// The child's session workflow stopped, so no run terminal will ever come.
    /// `reason` says how it stopped.
    SessionStopped {
        reason: String,
    },
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentLivenessActivityRequest {
    pub universe_id: Uuid,
    pub session_id: String,
}

/// Whether the child's session workflow is still there to finish its run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SubagentLivenessActivityResult {
    Running,
    Stopped { reason: String },
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
    pub resolution: Option<harness::PromiseResolution>,
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
    /// True only when the authoritative durable head was empty at bootstrap.
    /// This must not be inferred from tail replay counts because a checkpoint
    /// can make a non-empty session replay zero events.
    #[serde(default)]
    pub fresh_session: bool,
    /// Number of persisted tail events replayed during this bootstrap.
    #[serde(default)]
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

/// Application-failure type name for an append the session store refused because of
/// the data in it. The worker attaches it to a non-retryable `ApplicationFailure`.
/// Nothing was written, so the session workflow can discard those events and carry on.
pub const SESSION_DATA_REJECTED_ERROR_TYPE: &str = "session_data_rejected";

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
    pub request: harness::LlmGenerationRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompactActivityRequest {
    pub request: harness::ContextCompactionRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvokeBatchActivityRequest {
    pub request: harness::ToolInvocationBatchRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvokeCallActivityRequest {
    pub request: harness::ToolInvocationCallRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeToolPrepareScopeActivityRequest {
    pub tools: Vec<harness::ToolSpec>,
    pub model: harness::ModelSelection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeToolPrepareScopeActivityResult {
    pub bindings: BTreeMap<String, harness::CodeToolBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeToolInvokeActivityRequest {
    pub request: harness::ToolInvocationCallRequest,
}

/// Code tool waits belong to their execution scope; they never suspend or replace
/// the parent's conversational tool batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CodeToolInvokeActivityResult {
    Completed {
        result: harness::ToolInvocationResult,
    },
    Deferred {
        spec: harness::AwaitSpec,
    },
    EnvironmentNotReady {
        environment_id: String,
    },
}

/// Outcome of one per-call tool activity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolInvokeCallActivityResult {
    /// The call ran (or failed at tool level) and produced a terminal result.
    Completed {
        result: harness::ToolInvocationResult,
    },
    /// The call did not execute: the session's active environment is still
    /// provisioning or booting. The workflow waits for readiness with
    /// `await_environment_ready` and re-dispatches the same call.
    EnvironmentNotReady { environment_id: String },
    /// The runtime MCP call must receive a single-use run-owned decision
    /// before the worker performs any MCP wire I/O.
    NeedsApproval { subject: harness::ApprovalSubject },
}

/// Wait for the session's active environment to become reachable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitEnvironmentReadyActivityRequest {
    pub session_id: SessionId,
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_policy: Option<harness::EnvironmentsFeature>,
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
    pub request: harness::PromiseControlArgumentRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeProjectionRefreshActivityRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<harness::EnvironmentsFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_environment_id: Option<harness::EnvironmentId>,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_attachments: Vec<harness::WorkspaceAttachment>,
    pub vfs_catalog_enabled: bool,
    pub vfs_prompts_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfs_prompt_roots: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub active_instruction_inputs: BTreeMap<harness::ContextEntryKey, harness::ContextEntryInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfs_skills: Option<harness::VfsSkillsConfig>,
    /// Current keyed catalogs, including source provenance, for publication comparison.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub active_catalogs: BTreeMap<harness::ContextEntryKey, harness::ContextEntryInput>,
    /// The admitted sub-agent grant; the catalog entry follows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<harness::SubagentsFeature>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeProjectionRefreshActivityResult {
    pub commands: Vec<CoreAgentCommand>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const STALE_PAYLOAD: &str = "a payload already in Temporal history stopped decoding: give \
        the new field a serde default rather than changing this test";

    #[test]
    fn session_start_args_decode_from_their_identity_alone() {
        let args: AgentSessionArgs = serde_json::from_value(json!({
            "universe_id": "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f",
            "session_id": "session_mybot",
            "session_config": {
                "model": {
                    "api_kind": "open_ai_responses",
                    "provider_id": "openai",
                    "model": "gpt-test",
                },
            },
        }))
        .expect(STALE_PAYLOAD);
        assert_eq!(args.session_id, SessionId::new("session_mybot"));
        assert!(args.continuation_state.is_none());
    }

    #[test]
    fn continuation_state_decodes_from_nothing_at_the_current_version() {
        let state: AgentSessionContinuationState =
            serde_json::from_value(json!({})).expect(STALE_PAYLOAD);
        assert_eq!(state, AgentSessionContinuationState::default());
        assert_eq!(state.version, AgentSessionContinuationState::VERSION);
    }

    #[test]
    fn bootstrap_result_decodes_from_its_record_identity_alone() {
        let result: CreateOrLoadSessionResult = serde_json::from_value(json!({
            "record": {
                "session_id": "session_mybot",
                "retention_root_session_id": "session_mybot",
            },
        }))
        .expect(STALE_PAYLOAD);
        assert_eq!(result.record.session_id, SessionId::new("session_mybot"));
        assert_eq!(result.core_state, None);
        assert!(!result.fresh_session);
    }

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
        // A bare session id must not silently parse.
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
