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
    pub caller: CallerAccess,
}

/// What the calling credential may do, so a client can offer only the
/// methods it may call. Descriptive: core still checks every call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallerAccess {
    /// Display prefix of the calling key; absent for a request without a
    /// key, such as local development.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_prefix: Option<String>,
    /// The method groups the caller may call. A request without a key holds
    /// every group. `initialize` belongs to no group and is always allowed.
    pub groups: Vec<MethodGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    /// Release version with the git commit as SemVer build metadata, for
    /// example `0.1.0+2093b949…`.
    pub version: String,
    /// Full git commit this server was built from.
    pub git_sha: String,
    /// The environment daemon build this release ships. Descriptive: the
    /// gateway admits any daemon that speaks `envd.protocolVersion`,
    /// whatever commit it was built from.
    pub envd: EnvironmentDaemonInfo,
}

/// The environment daemon (`lightspeed-envd`) that belongs to a server
/// release. Download locations and checksums are not here: the deployment
/// publishes them in its discovery document, outside the authenticated API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentDaemonInfo {
    pub version: String,
    pub git_sha: String,
    /// Environment protocol version the gateway speaks. A daemon registers
    /// only when it reports exactly this number.
    pub protocol_version: u32,
    /// Rust target triples the daemon is published for.
    pub targets: Vec<String>,
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
