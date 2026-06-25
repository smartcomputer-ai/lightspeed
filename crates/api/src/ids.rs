use super::*;

pub type SessionId = String;
pub type RunId = String;
pub type ItemId = String;
pub type SkillId = String;
pub type EnvironmentId = String;
pub type EnvironmentProviderId = String;
pub type EnvironmentTargetId = String;

const SESSION_ID_MAX_LEN: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SessionIdError {
    #[error("session id must not be empty")]
    Empty,
    #[error("session id is too long: {actual} bytes, max {max}")]
    TooLong { max: usize, actual: usize },
    #[error("session id must start with an ASCII letter or digit")]
    InvalidStart,
    #[error(
        "session id contains invalid character {ch:?} at byte {index}; allowed: ASCII letters, digits, '_', '-', '.', ':'"
    )]
    InvalidCharacter { index: usize, ch: char },
}

pub fn validate_session_id(value: &str) -> Result<(), SessionIdError> {
    if value.is_empty() {
        return Err(SessionIdError::Empty);
    }
    if value.len() > SESSION_ID_MAX_LEN {
        return Err(SessionIdError::TooLong {
            max: SESSION_ID_MAX_LEN,
            actual: value.len(),
        });
    }
    let Some(first) = value.chars().next() else {
        return Err(SessionIdError::Empty);
    };
    if !first.is_ascii_alphanumeric() {
        return Err(SessionIdError::InvalidStart);
    }
    for (index, ch) in value.char_indices() {
        if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':')) {
            return Err(SessionIdError::InvalidCharacter { index, ch });
        }
    }
    Ok(())
}
