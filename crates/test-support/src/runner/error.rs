use engine::{
    CodecError, CommandError, CommandRejection, CoreAgentDriveError, DomainError, PlanningError,
    storage::{BlobStoreError, SessionStoreError},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RunnerError {
    #[error(transparent)]
    SessionStore(#[from] SessionStoreError),

    #[error(transparent)]
    BlobStore(#[from] BlobStoreError),

    #[error(transparent)]
    Codec(#[from] CodecError),

    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error(transparent)]
    Planning(#[from] PlanningError),

    #[error("command admission failed: {0}")]
    Command(CommandRejection),

    #[error("invalid runner request: {message}")]
    InvalidRequest { message: String },
}

impl From<CommandError> for RunnerError {
    fn from(error: CommandError) -> Self {
        match error {
            CommandError::Rejected(rejection) => Self::Command(rejection),
            CommandError::Domain(error) => Self::Domain(error),
        }
    }
}

impl From<CoreAgentDriveError> for RunnerError {
    fn from(error: CoreAgentDriveError) -> Self {
        match error {
            CoreAgentDriveError::Command(error) => error.into(),
            CoreAgentDriveError::Codec(error) => Self::Codec(error),
            CoreAgentDriveError::Domain(error) => Self::Domain(error),
            CoreAgentDriveError::Planning(error) => Self::Planning(error),
        }
    }
}
