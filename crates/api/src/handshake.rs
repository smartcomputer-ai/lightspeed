use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename = "AgentApiOutcomeOf{T}")]
pub struct AgentApiOutcome<T> {
    pub result: T,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notifications: Vec<AgentNotification>,
}

impl<T> AgentApiOutcome<T> {
    pub fn new(result: T) -> Self {
        Self {
            result,
            notifications: Vec::new(),
        }
    }

    pub fn with_notifications(result: T, notifications: Vec<AgentNotification>) -> Self {
        Self {
            result,
            notifications,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: Option<ClientInfo>,
    pub capabilities: Option<ClientCapabilities>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(default)]
    pub experimental_api: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: String,
    pub server_info: ServerInfo,
    pub capabilities: ServerCapabilities,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    pub notifications: bool,
    pub history_read: bool,
    #[serde(default)]
    pub event_log: bool,
    pub local_execution: bool,
}
