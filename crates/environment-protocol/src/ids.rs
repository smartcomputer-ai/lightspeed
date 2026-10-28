//! Typed identifiers.
//!
//! Every id space has its own type with a private field and a validating
//! decoder. Two grammars exist:
//!
//! - **General** (Lightspeed-assigned ids): 1 to 128 bytes, an ASCII letter
//!   or digit first, then ASCII letters, digits, `_`, `-`, `.` or `:`.
//! - **Opaque** (provider-assigned ids and daemon-minted tokens): 1 to 255
//!   bytes without control characters. Container, Kubernetes and cloud ids
//!   such as `default/pod-abc` or an ARN are valid as they are.
//!
//! [`JobName`] is a third, caller-chosen name space with its own rule.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

pub const GENERAL_ID_MAX_BYTES: u32 = 128;
pub const OPAQUE_ID_MAX_BYTES: u32 = 255;
pub const JOB_NAME_MAX_BYTES: u32 = 128;

/// JSON Schema pattern of the general grammar.
pub const GENERAL_ID_PATTERN: &str = "^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$";
/// JSON Schema pattern of a string without control characters (Unicode
/// category Cc), the character rule of opaque ids and job names.
pub const NO_CONTROL_PATTERN: &str = "^[^\\u0000-\\u001F\\u007F-\\u009F]+$";

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum IdError {
    #[error("{kind} must not be empty")]
    Empty { kind: &'static str },

    #[error("{kind} is {actual} bytes; the maximum is {max}")]
    TooLong {
        kind: &'static str,
        max: u32,
        actual: usize,
    },

    #[error("{kind} must start with an ASCII letter or digit")]
    InvalidStart { kind: &'static str },

    #[error(
        "{kind} contains {character:?} at byte {index}; allowed are ASCII letters, digits, '_', '-', '.', ':'"
    )]
    InvalidCharacter {
        kind: &'static str,
        index: usize,
        character: char,
    },

    #[error("{kind} contains a control character at byte {index}")]
    ControlCharacter { kind: &'static str, index: usize },
}

/// Check the general id grammar.
pub fn validate_general_id(kind: &'static str, value: &str) -> Result<(), IdError> {
    check_length(kind, value, GENERAL_ID_MAX_BYTES)?;
    if !value.as_bytes()[0].is_ascii_alphanumeric() {
        return Err(IdError::InvalidStart { kind });
    }
    if let Some((index, character)) = value
        .char_indices()
        .find(|(_, character)| !(character.is_ascii_alphanumeric() || "_-.:".contains(*character)))
    {
        return Err(IdError::InvalidCharacter {
            kind,
            index,
            character,
        });
    }
    Ok(())
}

/// Check the opaque id grammar.
pub fn validate_opaque_id(kind: &'static str, value: &str) -> Result<(), IdError> {
    check_length(kind, value, OPAQUE_ID_MAX_BYTES)?;
    check_no_control(kind, value)
}

/// Check the job name rule: 1 to [`JOB_NAME_MAX_BYTES`] bytes without
/// control characters.
pub fn validate_job_name(kind: &'static str, value: &str) -> Result<(), IdError> {
    check_length(kind, value, JOB_NAME_MAX_BYTES)?;
    check_no_control(kind, value)
}

fn check_no_control(kind: &'static str, value: &str) -> Result<(), IdError> {
    if let Some((index, _)) = value
        .char_indices()
        .find(|(_, character)| character.is_control())
    {
        return Err(IdError::ControlCharacter { kind, index });
    }
    Ok(())
}

fn check_length(kind: &'static str, value: &str, max: u32) -> Result<(), IdError> {
    if value.is_empty() {
        return Err(IdError::Empty { kind });
    }
    if value.len() > max as usize {
        return Err(IdError::TooLong {
            kind,
            max,
            actual: value.len(),
        });
    }
    Ok(())
}

macro_rules! protocol_id {
    ($(#[$doc:meta])* $name:ident, general) => {
        protocol_id!(
            $(#[$doc])* $name,
            validate_general_id,
            length(min = 1, max = 128),
            pattern(GENERAL_ID_PATTERN)
        );
    };
    ($(#[$doc:meta])* $name:ident, opaque) => {
        protocol_id!(
            $(#[$doc])* $name,
            validate_opaque_id,
            length(min = 1, max = 255),
            pattern(NO_CONTROL_PATTERN)
        );
    };
    ($(#[$doc:meta])* $name:ident, $validate:ident, $($schema:tt)*) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        pub struct $name(#[cfg_attr(feature = "schema", schemars($($schema)*))] String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                $validate(stringify!($name), &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = IdError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(D::Error::custom)
            }
        }
    };
}

protocol_id!(
    /// `processId`: caller-chosen handle of a process on one daemon.
    ProcessId,
    general
);
protocol_id!(
    /// `jobId`: caller-chosen name of one job run on one daemon.
    JobId,
    general
);
protocol_id!(
    /// `requestId` of a `job/start` call. Part of each job's identity: a
    /// retried start with the same request id and spec replays the job; any
    /// other reuse of the job id is refused.
    JobRequestId,
    general
);
protocol_id!(
    /// `queueKey`: jobs sharing a key run one at a time, in start order.
    QueueKey,
    general
);
protocol_id!(
    /// `operationId` of one transfer operation.
    TransferOperationId,
    general
);
protocol_id!(
    /// `daemonInstanceId`: random per daemon start. A change between two
    /// connections means the daemon restarted and every process handle from
    /// the earlier instance is gone.
    DaemonInstanceId,
    general
);
protocol_id!(
    /// `universeId` of a Lightspeed universe.
    UniverseId,
    general
);
protocol_id!(
    /// `environmentId` of a Lightspeed environment.
    EnvironmentId,
    general
);
protocol_id!(
    /// `daemonId` the gateway assigns to an admitted daemon identity.
    DaemonId,
    general
);
protocol_id!(
    /// `controlConnectionId` the gateway assigns to one control connection.
    ControlConnectionId,
    general
);
protocol_id!(
    /// `targetId`: the provider's own id for a target, opaque to Lightspeed.
    ProviderTargetId,
    opaque
);
protocol_id!(
    /// `templateId`: the provider's own id for a template.
    TemplateId,
    opaque
);
protocol_id!(
    /// `name` of a job and `dependsOn {kind: name}`: a caller-chosen name,
    /// unique within one `job/start`, 1 to 128 bytes without control
    /// characters.
    JobName,
    validate_job_name,
    length(min = 1, max = 128),
    pattern(NO_CONTROL_PATTERN)
);
protocol_id!(
    /// `cursor` and `nextCursor` of `fs/readDirectory`: an opaque position
    /// the daemon mints, echoed back unchanged.
    DirectoryCursor,
    opaque
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_ids_validate_on_decode() {
        assert!(serde_json::from_str::<JobId>("\"build.1:a-b_c\"").is_ok());
        for bad in ["\"\"", "\"-lead\"", "\"has space\"", "\"é\""] {
            assert!(serde_json::from_str::<ProcessId>(bad).is_err(), "{bad}");
        }
        let long = format!("\"{}\"", "a".repeat(GENERAL_ID_MAX_BYTES as usize + 1));
        assert!(serde_json::from_str::<ProcessId>(&long).is_err());
    }

    #[test]
    fn opaque_ids_accept_natural_provider_ids() {
        for good in [
            "default/pod-abc",
            "arn:aws:ec2:eu-west-1:123:instance/i-0abc",
            "ls-3f2a",
        ] {
            assert!(ProviderTargetId::new(good).is_ok(), "{good}");
        }
        assert!(ProviderTargetId::new("a\nb").is_err());
        assert!(ProviderTargetId::new("").is_err());
        assert!(ProviderTargetId::new("x".repeat(OPAQUE_ID_MAX_BYTES as usize + 1)).is_err());
    }

    #[test]
    fn job_names_allow_spaces_but_not_control_characters() {
        assert!(JobName::new("build & test").is_ok());
        assert!(JobName::new("a\tb").is_err());
        assert!(JobName::new("").is_err());
        assert!(JobName::new("x".repeat(JOB_NAME_MAX_BYTES as usize + 1)).is_err());
    }
}
