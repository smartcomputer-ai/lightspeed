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

impl DomainError {
    /// The refusal in its own words, for callers that report it to someone who
    /// did not ask about reducer internals.
    pub fn message(&self) -> &str {
        match self {
            Self::InvariantViolation(message)
            | Self::EventOrdering(message)
            | Self::ProviderCompatibility(message) => message,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CommandError {
    #[error("command rejected: {0}")]
    Rejected(CommandRejection),
    #[error(transparent)]
    Domain(#[from] DomainError),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{message}")]
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
    /// The session's lifecycle does not allow this command.
    SessionLifecycle,
    ActiveWork,
    MissingActiveRun,
    UnknownReference,
    /// The caller's input is wrong and would be refused in any state.
    Invalid,
    ProviderCompatibility,
    DuplicateSubmission,
    RevisionConflict,
    /// A fact this command would have to contradict is already recorded.
    Conflict,
    /// The input is fine, but what the session holds gives it nothing to do.
    Refused,
    /// The reducer refused what admission proposed. The caller did nothing
    /// wrong: this is a harness defect that cost one command.
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PlanningError {
    #[error("planning rejected: {0}")]
    Rejected(String),
    #[error(transparent)]
    Domain(#[from] DomainError),
}
