//! Controller-plane connection handshake.

use serde::{Deserialize, Serialize};

use crate::shared::{CURRENT_PROTOCOL_VERSION, ImplementationInfo};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerInitializeParams {
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
    pub client_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerInitializeResponse {
    pub protocol_version: u32,
    pub capabilities: ControllerCapabilities,
    pub implementation: ImplementationInfo,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerCapabilities {
    #[serde(default)]
    pub list_targets: bool,
    #[serde(default)]
    pub create_target: bool,
    #[serde(default)]
    pub attach_target: bool,
    #[serde(default)]
    pub get_target: bool,
    #[serde(default)]
    pub close_target: bool,
}

fn default_protocol_version() -> u32 {
    CURRENT_PROTOCOL_VERSION
}
