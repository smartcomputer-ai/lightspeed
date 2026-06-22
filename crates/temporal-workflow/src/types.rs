use std::collections::BTreeMap;

use engine::{
    BlobRef, ContextEntryInput, CoreAgentCommand, CoreAgentState, DynamicCommand, RunStatus,
    SessionConfig, SessionId, SessionPosition, SubmissionId,
    storage::{DynamicUncommittedSessionEvent, SessionRecord},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionArgs {
    pub session_id: SessionId,
    pub session_config: SessionConfig,
    pub instructions_ref: Option<BlobRef>,
    pub max_steps_per_input: Option<u32>,
    pub continue_as_new_history_threshold: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAdmission {
    pub command: DynamicCommand,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionStatus {
    pub session_id: String,
    pub initialized: bool,
    pub pending_admissions: usize,
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
    pub submission_id: Option<SubmissionId>,
    pub kind: AgentAdmissionFailureKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAdmissionFailureKind {
    InvalidCommand,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOrLoadSessionRequest {
    pub session_id: SessionId,
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
    pub events: Vec<DynamicUncommittedSessionEvent>,
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
pub struct SkillCatalogRefreshActivityRequest {
    pub session_id: SessionId,
    pub active_catalog_ref: Option<BlobRef>,
    pub active_vfs_catalog_ref: Option<BlobRef>,
    pub active_environment_catalog_ref: Option<BlobRef>,
    pub active_environment_active_ref: Option<BlobRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCatalogRefreshActivityResult {
    pub commands: Vec<CoreAgentCommand>,
}
