use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

macro_rules! string_id {
    ($name:ident, $validator:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                let value = value.into();
                Self::try_new(value)
                    .unwrap_or_else(|error| panic!("invalid {}: {error}", stringify!($name)))
            }

            pub fn try_new(value: impl Into<String>) -> Result<Self, StringIdError> {
                let value = value.into();
                $validator(stringify!($name), &value)?;
                Ok(Self(value))
            }

            pub fn parse(value: impl Into<String>) -> Result<Self, StringIdError> {
                Self::try_new(value)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = StringIdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = StringIdError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl FromStr for $name {
            type Err = StringIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_new(value).map_err(de::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

macro_rules! numeric_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
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
