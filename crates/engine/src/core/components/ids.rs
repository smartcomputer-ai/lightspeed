use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;
use std::str::FromStr;

pub use crate::session::{
    CorrelationId, EventSeq, SessionId, StringIdError, validate_general_string_id,
};

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

const TOOL_NAME_MAX_LEN: usize = 64;

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

string_id!(SubmissionId, validate_general_string_id);
string_id!(ContextEntryKey, validate_general_string_id);
string_id!(SkillId, validate_general_string_id);
string_id!(ToolCallId, validate_general_string_id);
string_id!(ToolName, validate_tool_name);
string_id!(WorkflowToolPortId, validate_general_string_id);
string_id!(
    WorkflowToolInvocationId,
    validate_workflow_tool_invocation_id
);

numeric_id!(RunId);
numeric_id!(MessageId);
numeric_id!(SteeringId);
numeric_id!(TurnId);
numeric_id!(ToolBatchId);
numeric_id!(ContextItemId);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdCursors {
    pub last_run_id: u64,
    pub last_message_id: u64,
    pub last_steering_id: u64,
    pub last_turn_id: u64,
    pub last_tool_batch_id: u64,
    pub last_context_item_id: u64,
}

fn validate_tool_name(kind: &'static str, value: &str) -> Result<(), StringIdError> {
    crate::session::validate_string_id_length(kind, value, TOOL_NAME_MAX_LEN)?;

    for (index, ch) in value.char_indices() {
        if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')) {
            return Err(StringIdError::InvalidCharacter {
                kind,
                index,
                ch,
                allowed: "ASCII letters, digits, '_', '-'",
            });
        }
    }
    Ok(())
}

fn validate_workflow_tool_invocation_id(
    kind: &'static str,
    value: &str,
) -> Result<(), StringIdError> {
    const PREFIX: &str = "wpi:sha256:";
    const DIGEST_LEN: usize = 64;
    crate::session::validate_string_id_length(kind, value, PREFIX.len() + DIGEST_LEN)?;
    if value.len() != PREFIX.len() + DIGEST_LEN
        || !value.starts_with(PREFIX)
        || !value[PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StringIdError::InvalidCharacter {
            kind,
            index: 0,
            ch: value.chars().next().unwrap_or('?'),
            allowed: "'wpi:sha256:' followed by 64 lowercase hexadecimal characters",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_string_ids_accept_portable_values() {
        assert_eq!(SessionId::new("session-1").as_str(), "session-1");
        assert_eq!(
            SubmissionId::new("client:submission_1").as_str(),
            "client:submission_1"
        );
    }

    #[test]
    fn string_ids_reject_non_portable_values() {
        assert!(matches!(
            SessionId::try_new(""),
            Err(StringIdError::Empty { kind: "SessionId" })
        ));
        assert!(matches!(
            SessionId::try_new("-session"),
            Err(StringIdError::InvalidStart { kind: "SessionId" })
        ));
        assert!(matches!(
            SessionId::try_new("session/name"),
            Err(StringIdError::InvalidCharacter {
                kind: "SessionId",
                ..
            })
        ));
        assert!(matches!(
            SessionId::try_new("session name"),
            Err(StringIdError::InvalidCharacter {
                kind: "SessionId",
                ..
            })
        ));
        assert!(matches!(
            SessionId::try_new("session🔥"),
            Err(StringIdError::InvalidCharacter {
                kind: "SessionId",
                ..
            })
        ));
    }

    #[test]
    fn tool_names_use_provider_safe_shape() {
        assert_eq!(ToolName::new("shell_tool-1").as_str(), "shell_tool-1");
        assert!(ToolName::try_new("tool.name").is_err());
        assert!(ToolName::try_new("tool:name").is_err());
        assert!(ToolName::try_new("tool/name").is_err());
        assert!(ToolName::try_new("tool name").is_err());
    }

    #[test]
    fn workflow_tool_invocation_ids_require_canonical_digest_shape() {
        let id = format!("wpi:sha256:{}", "a".repeat(64));
        assert_eq!(
            WorkflowToolInvocationId::try_new(id.clone())
                .expect("valid invocation id")
                .as_str(),
            id
        );
        assert!(
            WorkflowToolInvocationId::try_new(format!("wpi:sha256:{}", "A".repeat(64))).is_err()
        );
        assert!(WorkflowToolInvocationId::try_new("wpi:sha256:short").is_err());
    }

    #[test]
    fn string_id_new_panics_on_invalid_values() {
        let panic = std::panic::catch_unwind(|| SessionId::new("bad/id"));
        assert!(panic.is_err());
    }

    #[test]
    fn string_ids_round_trip_as_json_strings() {
        let id = SessionId::new("session-1");

        let encoded = serde_json::to_string(&id).expect("encode id");
        assert_eq!(encoded, "\"session-1\"");
        let decoded: SessionId = serde_json::from_str(&encoded).expect("decode id");
        assert_eq!(decoded, id);
    }

    #[test]
    fn string_id_deserialize_rejects_invalid_values() {
        assert!(serde_json::from_str::<SessionId>("\"session/name\"").is_err());
        assert!(serde_json::from_str::<ToolName>("\"tool.name\"").is_err());
    }
}
