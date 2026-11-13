use super::*;

/// Collections whose list rows can be watched. Each requires its existing API-key group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WatchKind {
    Session,
    Bot,
    Environment,
}

impl WatchKind {
    pub fn all() -> Vec<Self> {
        vec![Self::Session, Self::Bot, Self::Environment]
    }

    pub fn group(self) -> MethodGroup {
        match self {
            Self::Session => MethodGroup::Session,
            Self::Bot => MethodGroup::Bots,
            Self::Environment => MethodGroup::Environments,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UniverseWatchParams {
    /// Collections to watch; omission selects all three. Must be nonempty.
    #[serde(default = "WatchKind::all")]
    pub kinds: Vec<WatchKind>,
    /// Restrict session rows to shared sessions and sessions created by this actor,
    /// as on session/list. Includes delegated children through their root's access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_to: Option<String>,
}

/// Final response when a watch reaches its lifetime. Reconnect and refetch on
/// watch/reset; notifications are lossy hints and have no durable resume cursor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UniverseWatchResponse {}

pub const WATCH_NOTIFICATIONS: &[&str] = &[
    "session/changed", "session/removed", "bots/changed", "bots/removed",
    "environments/changed", "environments/removed", "watch/reset",
];
