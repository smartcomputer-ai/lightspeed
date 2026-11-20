use super::*;

/// Collections whose list rows can be watched. Each publishes its API family.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum WatchKind {
    Session,
    Bot,
    Environment,
    Workspace,
}

impl WatchKind {
    pub fn all() -> Vec<Self> {
        vec![Self::Session, Self::Bot, Self::Environment, Self::Workspace]
    }

    pub fn group(self) -> MethodGroup {
        match self {
            Self::Session => MethodGroup::Session,
            Self::Bot => MethodGroup::Bots,
            Self::Environment => MethodGroup::Environments,
            Self::Workspace => MethodGroup::Vfs,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UniverseWatchParams {
    /// Collections to watch; omission selects all four. Must be nonempty.
    #[serde(default = "WatchKind::all")]
    pub kinds: Vec<WatchKind>,
    /// Restrict session rows to shared sessions and sessions created by this actor,
    /// as on session/list. Includes delegated children through their root's access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_to: Option<String>,
}

pub const WATCH_NOTIFICATIONS: &[&str] = &[
    "session/changed",
    "session/removed",
    "bots/changed",
    "bots/removed",
    "environments/changed",
    "environments/removed",
    "workspaces/changed",
    "workspaces/removed",
    "watch/reset",
];
