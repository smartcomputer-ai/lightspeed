use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionParams {}

/// How the direct runtime connection authenticated. All runtime tokens have
/// full authority; permissions belong to the external gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallerAccess {
    /// True only for explicitly unauthenticated local development access.
    pub single: bool,
    /// Configured token name, absent for unauthenticated access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_name: Option<String>,
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
    /// The envd protocol version (data plane and registration channel)
    /// the gateway speaks. A daemon registers only when it reports exactly
    /// this number.
    pub protocol_version: u32,
    /// Rust target triples the daemon is published for.
    pub targets: Vec<String>,
}
