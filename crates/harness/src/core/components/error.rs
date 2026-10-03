use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum DomainError {
    #[error("domain invariant violation: {0}")]
    InvariantViolation(String),
    #[error("event ordering error: {0}")]
    EventOrdering(String),
    #[error("provider compatibility error: {0}")]
    ProviderCompatibility(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CommandError {
    #[error("command rejected: {0}")]
    Rejected(CommandRejection),
    #[error(transparent)]
    Domain(#[from] DomainError),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{kind:?}: {message}")]
pub struct CommandRejection {
    pub kind: CommandRejectionKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<CommandRejectionDetails>,
}

impl CommandRejection {
    pub fn new(kind: CommandRejectionKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
        }
    }

    pub fn context_revision_conflict(expected: u64, actual: u64) -> Self {
        Self {
            kind: CommandRejectionKind::RevisionConflict,
            message: format!("expected context revision {expected}, got {actual}"),
            details: Some(CommandRejectionDetails::ContextRevisionConflict { expected, actual }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandRejectionDetails {
    ContextRevisionConflict { expected: u64, actual: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandRejectionKind {
    CoreAgentState,
    ActiveWork,
    MissingActiveRun,
    UnknownReference,
    InvalidConfiguration,
    ProviderCompatibility,
    InvariantViolation,
    DuplicateSubmission,
    RevisionConflict,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PlanningError {
    #[error("planning rejected: {0}")]
    Rejected(String),
    #[error("invalid planning proposal: {0}")]
    InvalidProposal(String),
    #[error(transparent)]
    Domain(#[from] DomainError),
}
