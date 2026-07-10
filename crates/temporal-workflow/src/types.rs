use std::collections::BTreeMap;

use engine::{
    BlobRef, CommandRejection, ContextEntryInput, CoreAgentCommand, CoreAgentState, RunStatus,
    SessionConfig, SessionId, SessionPosition, SubmissionId, ToolBatchId,
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
    pub session_config: SessionConfig,
    pub max_steps_per_input: Option<u32>,
    pub continue_as_new_history_threshold: Option<u32>,
    #[serde(default)]
    pub close_on_terminal: bool,
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
    instance_id: &str,
    job_group_id: &str,
) -> String {
    format!("{universe_id}/envjob-{instance_id}-{job_group_id}")
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
    pub pending_admissions: usize,
    #[serde(default)]
    pub pending_tool_batch_resumes: usize,
    #[serde(default)]
    pub active_waits: usize,
    #[serde(default)]
    pub pending_promise_notifications: usize,
    pub active_run: Option<AgentActiveRunSummary>,
    pub queued_runs: Vec<AgentQueuedRunSummary>,
    pub completed_runs: Vec<AgentCompletedRunSummary>,
    #[serde(default)]
    pub consumed_message_submissions: Vec<AgentMessageSubmissionConsumptionSummary>,
    #[serde(default)]
    pub admission_failures: Vec<AgentAdmissionFailure>,
    pub last_error: Option<String>,
    /// True when the session workflow failed during bootstrap/rehydration. The
    /// gateway surfaces this as a typed `session_bootstrap_failed` error.
    #[serde(default)]
    pub bootstrap_failed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessageSubmissionConsumptionSummary {
    pub submission_id: SubmissionId,
    pub run_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAdmissionFailure {
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
    pub output_ref: Option<BlobRef>,
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
    pub output_ref: Option<BlobRef>,
    pub failure_message_ref: Option<BlobRef>,
}

/// Push-transport payload: the observed session signals the holder workflow
/// when a run carrying a notify-intent reaches a terminal state. The token is
/// the holder-side promise id — the edge event is the subscription (P92 §1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseResolutionSignal {
    pub token: String,
    pub status: RunStatus,
    pub output_ref: Option<BlobRef>,
    pub failure_message_ref: Option<BlobRef>,
}

/// Queued outbound notification on the observed side. Transient transport
/// state: the flush queue gates continue-as-new instead of being carried
/// through it, so delivery is at-least-once with idempotent receive keyed by
/// the promise id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPromiseNotification {
    pub holder_workflow_id: String,
    pub signal: PromiseResolutionSignal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPromiseCancellation {
    pub promise_id: String,
    pub source: engine::PromiseSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseSourcePoll {
    pub promise_id: String,
    pub source: engine::PromiseSource,
    pub next_check_at_ms: u64,
    pub poll_attempt: u32,
}

/// Total await outcome: every requested promise reports its state; timeout
/// is a successful return with partial results and the remaining promises
/// stay pending and re-awaitable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitOutput {
    pub outcome: AwaitOutcome,
    #[serde(default)]
    pub results: Vec<AwaitPromiseResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mailbox_messages: Vec<ContextEntryInput>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitOutcome {
    Terminal,
    Timeout,
    Cancelled,
    MailboxMessage,
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
    pub command: engine::ResumeAwaitCommand,
}

/// Armed while the active run sits in `cancelling`; the workflow forces the
/// run terminal once the deadline passes (P92 step 1 watchdog).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancellingWatchdog {
    pub run_id: u64,
    pub since_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseSourceResolutionSignal {
    pub promise_id: String,
    pub result: engine::PromiseSourceCheckResult,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<engine::RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<engine::TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<engine::ToolCallId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobCredentialScope {
    pub session_id: SessionId,
    pub env_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobStartActivityRequest {
    pub instance_id: String,
    pub job_group_id: String,
    pub request_ref: BlobRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobStartPayload {
    pub request: host_protocol::data::jobs::StartJobsParams,
    pub start_request_hash: String,
    #[serde(default)]
    pub provenance: EnvironmentJobProvenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_scope: Option<EnvironmentJobCredentialScope>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobStartActivityResult {
    pub jobs: Vec<host_protocol::data::jobs::JobSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobSubscription {
    pub holder_workflow_id: String,
    pub promise_id: String,
    pub job_id: host_protocol::shared::JobId,
    pub confirmation_deadline_ms: u64,
    #[serde(default)]
    pub confirmed: bool,
    #[serde(default)]
    pub notified: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobConfirmSubscriptionSignal {
    pub holder_workflow_id: String,
    pub promise_id: String,
    pub job_id: host_protocol::shared::JobId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobWorkflowArgs {
    pub start: EnvironmentJobStartActivityRequest,
    pub job_ids: Vec<host_protocol::shared::JobId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<EnvironmentJobSubscription>,
    #[serde(default)]
    pub started: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<host_protocol::data::jobs::JobSummary>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolutions: BTreeMap<String, engine::PromiseSourceCheckResult>,
    #[serde(default = "default_environment_job_poll_ms")]
    pub poll_ms: u64,
    #[serde(default)]
    pub poll_attempt: u32,
}

fn default_environment_job_poll_ms() -> u64 {
    2_000
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobWorkflowSnapshot {
    pub instance_id: String,
    pub job_group_id: String,
    #[serde(default)]
    pub started: bool,
    #[serde(default)]
    pub jobs: Vec<host_protocol::data::jobs::JobSummary>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolutions: BTreeMap<String, engine::PromiseSourceCheckResult>,
    pub terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobPollActivityRequest {
    pub instance_id: String,
    pub job_group_id: String,
    pub job_ids: Vec<host_protocol::shared::JobId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobPollActivityResult {
    pub jobs: Vec<host_protocol::data::jobs::JobSummary>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolutions: BTreeMap<String, engine::PromiseSourceCheckResult>,
    pub terminal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobCancelSignal {
    pub jobs: Vec<host_protocol::shared::JobId>,
    pub scope: host_protocol::data::jobs::JobCancelScope,
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentJobCancelActivityRequest {
    pub instance_id: String,
    pub jobs: Vec<host_protocol::shared::JobId>,
    pub scope: host_protocol::data::jobs::JobCancelScope,
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOrLoadSessionRequest {
    pub session_id: SessionId,
    /// Applied only when the session is created; ignored on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
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
    /// Number of persisted events replayed. `0` signals a fresh session that
    /// still needs `open_new_session`.
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
pub struct RuntimeProjectionRefreshActivityRequest {
    pub session_id: SessionId,
    pub vfs_catalog_enabled: bool,
    pub environment_catalog_enabled: bool,
    pub vfs_skills_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfs_skill_roots: Option<Vec<String>>,
    pub active_catalog_ref: Option<BlobRef>,
    pub active_vfs_catalog_ref: Option<BlobRef>,
    pub active_environment_catalog_ref: Option<BlobRef>,
    pub active_environment_active_ref: Option<BlobRef>,
    #[serde(default)]
    pub active_environment_target: Option<engine::ToolExecutionTarget>,
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
        // Pre-P90 ids were the bare session id; they must not silently parse.
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
