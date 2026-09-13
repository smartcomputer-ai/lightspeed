use crate::string_id::string_id;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

macro_rules! numeric_id {
    ($name:ident) => {
        #[derive(
            Clone,
            Copy,
            Debug,
            Default,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            Serialize,
            Deserialize,
        )]
        #[serde(transparent)]
        #[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
        pub struct $name(u64);

        impl $name {
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            pub const fn as_u64(self) -> u64 {
                self.0
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

const GENERAL_STRING_ID_MAX_LEN: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum StringIdError {
    #[error("{kind} must not be empty")]
    Empty { kind: &'static str },
    #[error("{kind} is too long: {actual} bytes, max {max}")]
    TooLong {
        kind: &'static str,
        max: usize,
        actual: usize,
    },
    #[error("{kind} must start with an ASCII letter or digit")]
    InvalidStart { kind: &'static str },
    #[error("{kind} contains invalid character {ch:?} at byte {index}; allowed: {allowed}")]
    InvalidCharacter {
        kind: &'static str,
        index: usize,
        ch: char,
        allowed: &'static str,
    },
}

string_id!(SessionId, validate_general_string_id);
string_id!(CorrelationId, validate_general_string_id);

numeric_id!(EventSeq);

pub fn validate_general_string_id(kind: &'static str, value: &str) -> Result<(), StringIdError> {
    validate_string_id_length(kind, value, GENERAL_STRING_ID_MAX_LEN)?;

    let Some(first) = value.chars().next() else {
        return Err(StringIdError::Empty { kind });
    };
    if !first.is_ascii_alphanumeric() {
        return Err(StringIdError::InvalidStart { kind });
    }

    for (index, ch) in value.char_indices() {
        if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':')) {
            return Err(StringIdError::InvalidCharacter {
                kind,
                index,
                ch,
                allowed: "ASCII letters, digits, '_', '-', '.', ':'",
            });
        }
    }
    Ok(())
}

pub fn validate_string_id_length(
    kind: &'static str,
    value: &str,
    max: usize,
) -> Result<(), StringIdError> {
    if value.is_empty() {
        return Err(StringIdError::Empty { kind });
    }
    if value.len() > max {
        return Err(StringIdError::TooLong {
            kind,
            max,
            actual: value.len(),
        });
    }
    Ok(())
}
