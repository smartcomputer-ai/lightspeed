use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", content = "params", rename_all = "camelCase")]
pub enum AgentNotification {
    #[serde(rename = "session/started")]
    SessionStarted { session: SessionView },
    #[serde(rename = "session/status/changed")]
    SessionStatusChanged {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        status: SessionStatus,
    },
    #[serde(rename = "session/event")]
    SessionEvent { event: SessionEventView },
    #[serde(rename = "run/started")]
    RunStarted {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        run: RunView,
    },
    #[serde(rename = "run/completed")]
    RunCompleted {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        run: RunView,
    },
    #[serde(rename = "item/completed")]
    ItemCompleted {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(rename = "runId")]
        run_id: RunId,
        item: SessionItemView,
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
            Self::SessionStarted { .. } => NOTIFY_SESSION_STARTED,
            Self::SessionStatusChanged { .. } => NOTIFY_SESSION_STATUS_CHANGED,
            Self::SessionEvent { .. } => NOTIFY_SESSION_EVENT,
            Self::RunStarted { .. } => NOTIFY_RUN_STARTED,
            Self::RunCompleted { .. } => NOTIFY_RUN_COMPLETED,
            Self::ItemCompleted { .. } => NOTIFY_ITEM_COMPLETED,
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
