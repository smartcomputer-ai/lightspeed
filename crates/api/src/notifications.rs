use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", content = "params", rename_all = "camelCase")]
pub enum AgentNotification {
    #[serde(rename = "session/changed")]
    SessionChanged { session: Box<SessionSummaryView> },
    #[serde(rename = "session/removed")]
    SessionRemoved {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
    },
    #[serde(rename = "bots/changed")]
    BotChanged { bot: Box<BotListItem> },
    #[serde(rename = "bots/removed")]
    BotRemoved {
        #[serde(rename = "botId")]
        bot_id: String,
    },
    #[serde(rename = "environments/changed")]
    EnvironmentChanged { environment: Box<EnvironmentView> },
    #[serde(rename = "environments/removed")]
    EnvironmentRemoved {
        #[serde(rename = "environmentId")]
        environment_id: String,
    },
    #[serde(rename = "workspaces/changed")]
    WorkspaceChanged { workspace: Box<VfsWorkspaceView> },
    #[serde(rename = "workspaces/removed")]
    WorkspaceRemoved {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
    },
    #[serde(rename = "watch/reset")]
    WatchReset {},
}
