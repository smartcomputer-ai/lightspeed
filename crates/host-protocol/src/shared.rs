//! Shared protocol primitives.

use std::{fmt, str::FromStr};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as SerdeError};
use thiserror::Error;

pub const CURRENT_PROTOCOL_VERSION: u32 = 1;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }
    };
}

string_id!(HostTargetId);
string_id!(HostConnectionId);
string_id!(ProcessId);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HostTransport {
    WebSocket,
    Http,
    Stdio,
    Ssh,
    Provider {
        #[serde(rename = "providerType")]
        provider_type: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostConnectionSpec {
    pub target_id: HostTargetId,
    pub endpoint: String,
    pub transport: HostTransport,
    pub scope: HostScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cwd: Option<HostPath>,
    pub capabilities: HostCapabilities,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HostScope {
    Default,
    #[serde(rename_all = "camelCase")]
    Session {
        session_id: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCapabilities {
    #[serde(default)]
    pub filesystem_read: bool,
    #[serde(default)]
    pub filesystem_write: bool,
    #[serde(default)]
    pub process_start: bool,
    #[serde(default)]
    pub process_stdin: bool,
    #[serde(default)]
    pub process_terminate: bool,
    #[serde(default)]
    pub process_output_polling: bool,
    #[serde(default)]
    pub process_output_notifications: bool,
    #[serde(default)]
    pub process_pty: bool,
}

impl HostCapabilities {
    pub fn filesystem(read: bool, write: bool) -> Self {
        Self {
            filesystem_read: read,
            filesystem_write: write,
            ..Self::default()
        }
    }

    pub fn with_process(mut self) -> Self {
        self.process_start = true;
        self.process_stdin = true;
        self.process_terminate = true;
        self.process_output_polling = true;
        self.process_output_notifications = true;
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImplementationInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ByteChunk(pub Vec<u8>);

impl ByteChunk {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for ByteChunk {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}

impl From<&[u8]> for ByteChunk {
    fn from(value: &[u8]) -> Self {
        Self(value.to_vec())
    }
}

impl Serialize for ByteChunk {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&BASE64_STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for ByteChunk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        BASE64_STANDARD
            .decode(encoded)
            .map(Self)
            .map_err(SerdeError::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostPath {
    normalized: String,
}

impl HostPath {
    pub fn new(path: impl AsRef<str>) -> Result<Self, HostPathError> {
        normalize_path(path.as_ref()).map(|normalized| Self { normalized })
    }

    pub fn root() -> Self {
        Self {
            normalized: "/".to_owned(),
        }
    }

    pub fn current_dir() -> Self {
        Self {
            normalized: ".".to_owned(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.normalized
    }

    pub fn is_absolute(&self) -> bool {
        self.normalized.starts_with('/')
    }
}

impl fmt::Display for HostPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.normalized.fmt(formatter)
    }
}

impl FromStr for HostPath {
    type Err = HostPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for HostPath {
    type Error = HostPathError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for HostPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HostPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(SerdeError::custom)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HostPathError {
    #[error("host path is empty")]
    Empty,

    #[error("host path contains a NUL byte")]
    NulByte,

    #[error("host path must use '/' separators: {path}")]
    NonUnixSeparator { path: String },
}

fn normalize_path(path: &str) -> Result<String, HostPathError> {
    if path.is_empty() {
        return Err(HostPathError::Empty);
    }
    if path.contains('\0') {
        return Err(HostPathError::NulByte);
    }
    if path.contains('\\') {
        return Err(HostPathError::NonUnixSeparator {
            path: path.to_owned(),
        });
    }

    let absolute = path.starts_with('/');
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if let Some(last) = segments.last()
                    && *last != ".."
                {
                    segments.pop();
                    continue;
                }
                if !absolute {
                    segments.push("..");
                }
            }
            segment => segments.push(segment),
        }
    }

    if absolute {
        if segments.is_empty() {
            Ok("/".to_owned())
        } else {
            Ok(format!("/{}", segments.join("/")))
        }
    } else if segments.is_empty() {
        Ok(".".to_owned())
    } else {
        Ok(segments.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_chunk_serializes_as_base64_string() {
        let value = serde_json::to_value(ByteChunk::from(b"hello".as_slice())).expect("serialize");

        assert_eq!(value, serde_json::json!("aGVsbG8="));
    }

    #[test]
    fn host_path_normalizes_like_agent_fs_path() {
        let path = HostPath::new("src//./lib/../main.rs").expect("path");

        assert_eq!(path.as_str(), "src/main.rs");
    }
}
