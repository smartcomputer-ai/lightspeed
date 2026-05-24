use serde::{Deserialize, Serialize};

pub use crate::session::SessionPosition;
use crate::{
    CoreAgentEvent, CorrelationId, RunId, SubmissionId, ToolBatchId, ToolCallId, TurnId,
    session::SessionEntry as GenericSessionEntry,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreAgentJoins {
    pub run_id: Option<RunId>,
    pub turn_id: Option<TurnId>,
    pub tool_batch_id: Option<ToolBatchId>,
    pub tool_call_id: Option<ToolCallId>,
    pub submission_id: Option<SubmissionId>,
    pub correlation_id: Option<CorrelationId>,
}

pub type CoreAgentEntry = GenericSessionEntry<CoreAgentEvent, CoreAgentJoins>;

pub type UncommittedCoreAgentEvent =
    crate::session::UncommittedSessionEvent<CoreAgentEvent, CoreAgentJoins>;
