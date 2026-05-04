//! Data-plane connection handshake.

use serde::{Deserialize, Serialize};

use crate::shared::{
    CURRENT_PROTOCOL_VERSION, HostCapabilities, HostConnectionId, HostScope, ImplementationInfo,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
    pub client_name: String,
    pub scope: HostScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_connection_id: Option<HostConnectionId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u32,
    pub connection_id: HostConnectionId,
    pub capabilities: HostCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cwd: Option<String>,
    pub implementation: ImplementationInfo,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializedParams {}

fn default_protocol_version() -> u32 {
    CURRENT_PROTOCOL_VERSION
}
