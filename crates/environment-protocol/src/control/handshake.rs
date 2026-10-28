//! Controller-plane `initialize`.
//!
//! The same handshake rules as the data plane apply: `initialize` first
//! (`notInitialized` otherwise), once (`invalidRequest` on a repeat), with
//! [`CONTROLLER_PROTOCOL_VERSION`](crate::CONTROLLER_PROTOCOL_VERSION)
//! (`versionMismatch` otherwise). `template/list`, `target/create`,
//! `target/get` and `target/close` are mandatory at this version; the
//! capabilities name the optional ones.

use serde::{Deserialize, Serialize};

use crate::{
    handshake::{ImplementationInfo, Plane},
    serde_util::is_false,
};

/// `initialize` params on the controller plane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControllerInitializeParams {
    /// The client's `CONTROLLER_PROTOCOL_VERSION`.
    pub protocol_version: u32,
    pub client_info: ImplementationInfo,
}

/// `initialize` response on the controller plane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControllerInitializeResponse {
    pub protocol_version: u32,
    /// Always `controller`.
    pub plane: Plane,
    pub server_info: ImplementationInfo,
    pub capabilities: ControllerCapabilities,
}

/// Optional controller methods. Calling an absent one is
/// `capabilityUnavailable`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControllerCapabilities {
    /// `target/setPower` is served.
    #[serde(default, skip_serializing_if = "is_false")]
    pub power: bool,
    /// `ingress/ensure` and `ingress/remove` are served.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ingress: bool,
}
