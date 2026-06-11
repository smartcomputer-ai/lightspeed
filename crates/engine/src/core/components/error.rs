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
}

impl CommandRejection {
    pub fn new(kind: CommandRejectionKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
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
