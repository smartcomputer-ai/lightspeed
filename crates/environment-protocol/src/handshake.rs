//! Values every handshake shares.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[cfg(feature = "schema")]
use crate::ids::NO_CONTROL_PATTERN;
use crate::limits::MAX_IMPLEMENTATION_FIELD_BYTES;

/// The plane a connection serves. Each `initialize` response names its
/// plane, so a client that reached the wrong socket fails at the handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum Plane {
    /// envd's data plane, versioned by `ENVD_PROTOCOL_VERSION`.
    Data,
    /// A provider controller, versioned by `CONTROLLER_PROTOCOL_VERSION`.
    Controller,
    /// The registration channel between a daemon and the gateway, versioned
    /// by `ENVD_PROTOCOL_VERSION`.
    Registration,
}

impl Plane {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Controller => "controller",
            Self::Registration => "registration",
        }
    }
}

impl std::fmt::Display for Plane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What a peer says about its own build: `clientInfo`, `serverInfo` and a
/// daemon's registration `implementation`. Descriptive only; the protocol
/// version alone decides whether two peers can talk, and nothing here
/// selects, authenticates or gates. Every field is 1 to 256 bytes without control characters,
/// checked on decode and by every constructor, so a value that would be
/// refused can never be built or sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImplementationInfo {
    /// Product name, such as `lightspeed-envd`.
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 256), pattern(NO_CONTROL_PATTERN))
    )]
    name: String,
    /// Release version, such as `0.5.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 256), pattern(NO_CONTROL_PATTERN))
    )]
    version: Option<String>,
    /// Full or abbreviated git commit of the build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 256), pattern(NO_CONTROL_PATTERN))
    )]
    git_sha: Option<String>,
    /// Rust target triple of the build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 256), pattern(NO_CONTROL_PATTERN))
    )]
    target: Option<String>,
}

/// The decoded shape before the bounds are checked.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImplementationInfoFields {
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    git_sha: Option<String>,
    #[serde(default)]
    target: Option<String>,
}

impl ImplementationInfoFields {
    fn into_info(self) -> Result<ImplementationInfo, ImplementationInfoError> {
        let info = ImplementationInfo {
            name: self.name,
            version: self.version,
            git_sha: self.git_sha,
            target: self.target,
        };
        check_field("name", Some(&info.name))?;
        check_field("version", info.version.as_deref())?;
        check_field("gitSha", info.git_sha.as_deref())?;
        check_field("target", info.target.as_deref())?;
        Ok(info)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{field} must be 1..={max} bytes without control characters", max = MAX_IMPLEMENTATION_FIELD_BYTES)]
pub struct ImplementationInfoError {
    /// Wire name of the offending member.
    pub field: &'static str,
}

impl ImplementationInfo {
    pub fn new(name: impl Into<String>) -> Result<Self, ImplementationInfoError> {
        let name = name.into();
        check_field("name", Some(&name))?;
        Ok(Self {
            name,
            version: None,
            git_sha: None,
            target: None,
        })
    }

    pub fn with_version(
        mut self,
        version: impl Into<String>,
    ) -> Result<Self, ImplementationInfoError> {
        let version = version.into();
        check_field("version", Some(&version))?;
        self.version = Some(version);
        Ok(self)
    }

    pub fn with_git_sha(
        mut self,
        git_sha: impl Into<String>,
    ) -> Result<Self, ImplementationInfoError> {
        let git_sha = git_sha.into();
        check_field("gitSha", Some(&git_sha))?;
        self.git_sha = Some(git_sha);
        Ok(self)
    }

    pub fn with_target(
        mut self,
        target: impl Into<String>,
    ) -> Result<Self, ImplementationInfoError> {
        let target = target.into();
        check_field("target", Some(&target))?;
        self.target = Some(target);
        Ok(self)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    pub fn git_sha(&self) -> Option<&str> {
        self.git_sha.as_deref()
    }

    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    /// For a value that has the shape of an `ImplementationInfo` but breaks
    /// its bounds, the offending member. `None` when the value decodes, or
    /// when it does not even have the shape (a strict decode reports that).
    pub fn bounds_violation(value: &Value) -> Option<ImplementationInfoError> {
        ImplementationInfoFields::deserialize(value)
            .ok()?
            .into_info()
            .err()
    }
}

impl<'de> Deserialize<'de> for ImplementationInfo {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        ImplementationInfoFields::deserialize(deserializer)?
            .into_info()
            .map_err(serde::de::Error::custom)
    }
}

fn check_field(field: &'static str, value: Option<&str>) -> Result<(), ImplementationInfoError> {
    match value {
        Some(value)
            if value.is_empty()
                || value.len() > MAX_IMPLEMENTATION_FIELD_BYTES as usize
                || value.chars().any(char::is_control) =>
        {
            Err(ImplementationInfoError { field })
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implementation_info_is_bounded_on_decode() {
        let ok = serde_json::json!({"name": "lightspeed-envd", "version": "0.5.0"});
        assert!(serde_json::from_value::<ImplementationInfo>(ok).is_ok());
        for bad in [
            serde_json::json!({"name": ""}),
            serde_json::json!({"name": "x", "version": "a\nb"}),
            serde_json::json!({"name": "x", "gitSha": "a".repeat(257)}),
            serde_json::json!({"name": "x", "extra": 1}),
        ] {
            assert!(serde_json::from_value::<ImplementationInfo>(bad).is_err());
        }
    }

    #[test]
    fn constructors_enforce_the_decode_bounds() {
        let info = ImplementationInfo::new("lightspeed-envd")
            .and_then(|info| info.with_version("0.5.0"))
            .unwrap();
        assert_eq!(info.version(), Some("0.5.0"));
        assert_eq!(
            ImplementationInfo::new(""),
            Err(ImplementationInfoError { field: "name" })
        );
        assert_eq!(
            info.with_git_sha("a\nb"),
            Err(ImplementationInfoError { field: "gitSha" })
        );
    }

    #[test]
    fn bounds_violations_are_told_apart_from_shape_errors() {
        let long = serde_json::json!({"name": "x", "version": "v".repeat(257)});
        assert_eq!(
            ImplementationInfo::bounds_violation(&long),
            Some(ImplementationInfoError { field: "version" })
        );
        assert_eq!(
            ImplementationInfo::bounds_violation(&serde_json::json!({"name": 1})),
            None
        );
        assert_eq!(
            ImplementationInfo::bounds_violation(&serde_json::json!({"name": "ok"})),
            None
        );
    }
}
