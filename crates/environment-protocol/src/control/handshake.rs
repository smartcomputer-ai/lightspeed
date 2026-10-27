//! Controller-plane `initialize`.
//!
//! The same handshake rules as the data plane apply: `initialize` first
//! (`notInitialized` otherwise), once (`invalidRequest` on a repeat), with
//! [`CONTROLLER_PROTOCOL_VERSION`](crate::CONTROLLER_PROTOCOL_VERSION)
//! (`versionMismatch` otherwise). The core methods (`template/list` and
//! every `target/*` method except `target/adopt`) are mandatory at this
//! version; the capabilities name the optional ones.

use serde::{Deserialize, Serialize};

use crate::{
    handshake::{ImplementationInfo, Plane},
    routes::DataRouteBase,
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
    pub routes: ControllerRoutes,
}

/// Optional controller methods. Calling an absent one is
/// `capabilityUnavailable`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControllerCapabilities {
    /// `target/adopt` is served.
    #[serde(default, skip_serializing_if = "is_false")]
    pub adopt_target: bool,
    /// `ingress/ensure` and `ingress/remove` are served.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ingress: bool,
}

/// Where Lightspeed reaches a target's data plane through this provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControllerRoutes {
    /// Base URL of the data routes. A target's data route is this base
    /// followed by
    /// `/{universeId}/{bindingId}/{environmentId}/{incarnationId}/{targetId}`,
    /// each segment percent-encoded
    /// ([`provider_data_route`](crate::routes::provider_data_route)), and is
    /// dialed with the same bearer credential as the controller. A
    /// controller reached over `wss` must advertise a `wss` base.
    pub data: DataRouteBase,
}
