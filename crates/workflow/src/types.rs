use engine::{
    BlobRef, ContextEntryInput, CoreAgentCommand, DynamicCommand, RunStatus, SessionConfig,
    SessionId, SessionPosition, SubmissionId,
    storage::{DynamicSessionEntry, DynamicUncommittedSessionEvent, SessionRecord},
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateOrLoadSessionResult {
    pub record: SessionRecord,
    pub entries: Vec<DynamicSessionEntry>,
}

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
pub struct ToolInvokeBatchActivityRequest {
    pub request: engine::ToolInvocationBatchRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCatalogRefreshActivityRequest {
    pub session_id: SessionId,
    pub active_catalog_ref: Option<BlobRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCatalogRefreshActivityResult {
    pub command: Option<CoreAgentCommand>,
}
