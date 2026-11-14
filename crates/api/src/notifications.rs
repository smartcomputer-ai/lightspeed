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
    #[serde(rename = "session/started")]
    SessionStarted { session: Box<SessionView> },
    #[serde(rename = "session/event")]
    SessionEvent { event: Box<SessionEventView> },
    #[serde(rename = "session/runs/started")]
    RunStarted {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        run: RunView,
    },
    #[serde(rename = "session/runs/completed")]
    RunCompleted {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        run: RunView,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "sessionId")]
        session_id: Option<SessionId>,
        message: String,
    },
}

impl AgentNotification {
    pub fn method(&self) -> &'static str {
        match self {
            Self::SessionChanged { .. } => "session/changed",
            Self::SessionRemoved { .. } => "session/removed",
            Self::BotChanged { .. } => "bots/changed",
            Self::BotRemoved { .. } => "bots/removed",
            Self::EnvironmentChanged { .. } => "environments/changed",
            Self::EnvironmentRemoved { .. } => "environments/removed",
            Self::WorkspaceChanged { .. } => "workspaces/changed",
            Self::WorkspaceRemoved { .. } => "workspaces/removed",
            Self::WatchReset { .. } => "watch/reset",
            Self::SessionStarted { .. } => NOTIFY_SESSION_STARTED,
            Self::SessionEvent { .. } => NOTIFY_SESSION_EVENT,
            Self::RunStarted { .. } => NOTIFY_SESSION_RUNS_STARTED,
            Self::RunCompleted { .. } => NOTIFY_SESSION_RUNS_COMPLETED,
            Self::Error { .. } => NOTIFY_ERROR,
        }
    }

    pub fn into_json_rpc(self) -> Result<JsonRpcNotification, serde_json::Error> {
        let method = self.method().to_owned();
        let value = serde_json::to_value(self)?;
        let params = value.get("params").cloned();
        Ok(JsonRpcNotification { method, params })
    }
}
