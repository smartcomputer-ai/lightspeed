//! Client-facing API contracts for Forge agents.
//!
//! This crate is intentionally independent of `agent-core` core types. Hosts
//! can implement these contracts from a local event-log runner, a Temporal
//! workflow gateway, or another substrate while clients keep speaking the same
//! session/run/item protocol.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const PROTOCOL_VERSION: &str = "forge.agent.api.v1";

pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_SESSION_START: &str = "session/start";
pub const METHOD_SESSION_READ: &str = "session/read";
pub const METHOD_SESSION_EVENTS_READ: &str = "session/events/read";
pub const METHOD_RUN_START: &str = "run/start";

pub const NOTIFY_SESSION_STARTED: &str = "session/started";
pub const NOTIFY_SESSION_STATUS_CHANGED: &str = "session/status/changed";
pub const NOTIFY_SESSION_EVENT: &str = "session/event";
pub const NOTIFY_RUN_STARTED: &str = "run/started";
pub const NOTIFY_RUN_COMPLETED: &str = "run/completed";
pub const NOTIFY_ITEM_COMPLETED: &str = "item/completed";
pub const NOTIFY_ERROR: &str = "error";

pub type SessionId = String;
pub type RunId = String;
pub type ItemId = String;

#[async_trait]
pub trait AgentApiService: Send + Sync {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> Result<AgentApiOutcome<InitializeResponse>, AgentApiError>;

    async fn start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError>;

    async fn read_session(
        &self,
        params: SessionReadParams,
    ) -> Result<AgentApiOutcome<SessionReadResponse>, AgentApiError>;

    async fn read_session_events(
        &self,
        params: SessionEventsReadParams,
    ) -> Result<AgentApiOutcome<SessionEventsReadResponse>, AgentApiError>;

    async fn start_run(
        &self,
        params: RunStartParams,
    ) -> Result<AgentApiOutcome<RunStartResponse>, AgentApiError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: Option<ClientInfo>,
    pub capabilities: Option<ClientCapabilities>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(default)]
    pub experimental_api: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: String,
    pub server_info: ServerInfo,
    pub capabilities: ServerCapabilities,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    pub notifications: bool,
    pub history_read: bool,
    #[serde(default)]
    pub event_log: bool,
    pub local_execution: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartParams {
    pub session_id: Option<SessionId>,
    pub cwd: Option<String>,
    pub model: Option<ModelConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadResponse {
    pub session: SessionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventsReadParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<EventCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventsReadResponse {
    #[serde(default)]
    pub events: Vec<SessionEventView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<EventCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_cursor: Option<EventCursor>,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<EventLogGap>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventCursor {
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLogGap {
    pub requested_after: Option<EventCursor>,
    pub retained_after: Option<EventCursor>,
    pub next_cursor: Option<EventCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventView {
    pub cursor: EventCursor,
    pub session_id: SessionId,
    pub observed_at_ms: u64,
    pub joins: EventJoinsView,
    pub kind: SessionEventKindView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventJoinsView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionEventKindView {
    SessionOpened {
        model: Option<ModelConfig>,
    },
    SessionConfigChanged {
        model: Option<ModelConfig>,
        revision: u64,
    },
    SessionClosed,
    RunQueued {
        submission_id: Option<String>,
        input_ref: String,
    },
    RunStarted {
        run_id: RunId,
        submission_id: Option<String>,
        input_ref: String,
    },
    RunSteeringAdded {
        run_id: RunId,
        input_ref: String,
    },
    RunCancellationRequested {
        run_id: RunId,
    },
    RunCompleted {
        run_id: RunId,
        output_ref: Option<String>,
    },
    RunFailed {
        run_id: RunId,
        message: String,
    },
    RunCancelled {
        run_id: RunId,
    },
    TurnStarted {
        run_id: RunId,
        turn_id: String,
    },
    TurnPlanned {
        run_id: RunId,
        turn_id: String,
    },
    TurnGenerationRequested {
        run_id: RunId,
        turn_id: String,
    },
    TurnGenerationCompleted {
        run_id: RunId,
        turn_id: String,
        status: String,
    },
    TurnCompleted {
        turn_id: String,
    },
    ItemsRecorded {
        items: Vec<SessionItemView>,
    },
    ContextWindowPlanned {
        run_id: RunId,
        turn_id: String,
    },
    CompactionRecorded {
        run_id: Option<RunId>,
        turn_id: Option<String>,
        summary_ref: Option<String>,
    },
    ToolRegistryChanged,
    ToolProfileSelected {
        profile_id: String,
    },
    ToolDefaultTargetChanged {
        namespace: String,
        target: Option<ToolExecutionTargetView>,
    },
    ToolBatchStarted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        calls: Vec<ToolCallEventView>,
    },
    ToolCallStarted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        call_id: String,
    },
    ToolCallCompleted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        call_id: String,
        status: ToolItemStatus,
    },
    ToolBatchCompleted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionTargetView {
    pub namespace: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEventView {
    pub call_id: String,
    pub tool_name: String,
    pub arguments_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<ToolCallDisplayView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartParams {
    pub session_id: SessionId,
    pub input: Vec<InputItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartResponse {
    pub run: RunView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    pub provider_id: String,
    pub api_kind: String,
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: SessionId,
    pub status: SessionStatus,
    pub cwd: Option<String>,
    pub model: Option<ModelConfig>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub runs: Vec<RunView>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    NotLoaded,
    Idle,
    Active,
    Closed,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub id: RunId,
    pub status: RunStatus,
    pub input: Vec<InputItem>,
    #[serde(default)]
    pub items: Vec<SessionItemView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_batches: Vec<ToolBatchView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolBatchView {
    pub id: String,
    pub turn_id: String,
    pub status: ToolItemStatus,
    #[serde(default)]
    pub calls: Vec<ToolCallView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallView {
    pub call_id: String,
    pub tool_name: String,
    pub arguments_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    pub status: ToolItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<ToolCallDisplayView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallDisplayView {
    pub group: ToolCallDisplayGroup,
    pub verb: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallDisplayGroup {
    Explore,
    Edit,
    Execute,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunStatus {
    Queued,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum InputItem {
    Text { text: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionItemView {
    UserMessage {
        id: ItemId,
        text: String,
    },
    AssistantMessage {
        id: ItemId,
        text: String,
    },
    ToolCall {
        id: ItemId,
        call_id: String,
        tool_name: String,
        arguments: Option<String>,
        status: ToolItemStatus,
    },
    ToolResult {
        id: ItemId,
        call_id: String,
        output: Option<String>,
        is_error: bool,
        status: ToolItemStatus,
    },
    SystemEvent {
        id: ItemId,
        text: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolItemStatus {
    Requested,
    Running,
    Succeeded,
    Failed,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentApiErrorKind {
    InvalidRequest,
    NotFound,
    Conflict,
    Rejected,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{kind:?}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct AgentApiError {
    pub kind: AgentApiErrorKind,
    pub message: String,
}

impl AgentApiError {
    pub fn new(kind: AgentApiErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::InvalidRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::NotFound, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::Conflict, message)
    }

    pub fn rejected(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::Rejected, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(AgentApiErrorKind::Internal, message)
    }

    pub fn json_rpc_code(&self) -> i64 {
        match self.kind {
            AgentApiErrorKind::InvalidRequest => -32602,
            AgentApiErrorKind::NotFound => -32004,
            AgentApiErrorKind::Conflict => -32009,
            AgentApiErrorKind::Rejected => -32010,
            AgentApiErrorKind::Internal => -32603,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(u64),
    String(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcRequest {
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcNotification {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcResponse {
    pub id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl From<AgentApiError> for JsonRpcError {
    fn from(error: AgentApiError) -> Self {
        Self {
            code: error.json_rpc_code(),
            message: error.message,
            data: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn notification_serializes_as_json_rpc_lite_shape() {
        let notification = AgentNotification::RunCompleted {
            session_id: "session_1".to_owned(),
            run: RunView {
                id: "run_1".to_owned(),
                status: RunStatus::Completed,
                input: vec![InputItem::Text {
                    text: "hello".to_owned(),
                }],
                items: Vec::new(),
                tool_batches: Vec::new(),
            },
        };

        let value = serde_json::to_value(notification).expect("serialize notification");

        assert_eq!(
            value,
            json!({
                "method": "run/completed",
                "params": {
                    "sessionId": "session_1",
                    "run": {
                        "id": "run_1",
                        "status": "completed",
                        "input": [{ "type": "text", "text": "hello" }],
                        "items": []
                    }
                }
            })
        );
    }

    #[test]
    fn request_ids_accept_number_or_string() {
        let numeric: JsonRpcRequest = serde_json::from_value(json!({
            "id": 7,
            "method": "session/start"
        }))
        .expect("numeric id");
        let string: JsonRpcRequest = serde_json::from_value(json!({
            "id": "req_7",
            "method": "session/start"
        }))
        .expect("string id");

        assert_eq!(numeric.id, RequestId::Number(7));
        assert_eq!(string.id, RequestId::String("req_7".to_owned()));
    }

    #[test]
    fn session_event_serializes_with_cursor_and_kind() {
        let event = SessionEventView {
            cursor: EventCursor { seq: 3 },
            session_id: "session_1".to_owned(),
            observed_at_ms: 12,
            joins: EventJoinsView {
                run_id: Some("run_1".to_owned()),
                ..EventJoinsView::default()
            },
            kind: SessionEventKindView::RunCompleted {
                run_id: "run_1".to_owned(),
                output_ref: Some("sha256:abc".to_owned()),
            },
        };

        let value = serde_json::to_value(AgentNotification::SessionEvent { event })
            .expect("serialize event notification");

        assert_eq!(
            value,
            json!({
                "method": "session/event",
                "params": {
                    "event": {
                        "cursor": { "seq": 3 },
                        "sessionId": "session_1",
                        "observedAtMs": 12,
                        "joins": { "runId": "run_1" },
                        "kind": {
                            "type": "runCompleted",
                            "runId": "run_1",
                            "outputRef": "sha256:abc"
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn tool_batch_started_event_can_inline_tool_arguments() {
        let event = SessionEventView {
            cursor: EventCursor { seq: 4 },
            session_id: "session_1".to_owned(),
            observed_at_ms: 12,
            joins: EventJoinsView {
                run_id: Some("run_1".to_owned()),
                tool_batch_id: Some("tool_batch_1".to_owned()),
                ..EventJoinsView::default()
            },
            kind: SessionEventKindView::ToolBatchStarted {
                run_id: "run_1".to_owned(),
                turn_id: "turn_1".to_owned(),
                batch_id: "tool_batch_1".to_owned(),
                calls: vec![ToolCallEventView {
                    call_id: "call_1".to_owned(),
                    tool_name: "read_file".to_owned(),
                    arguments_ref: "sha256:args".to_owned(),
                    arguments: Some(r#"{"path":"README.md"}"#.to_owned()),
                    display: Some(ToolCallDisplayView {
                        group: ToolCallDisplayGroup::Explore,
                        verb: "Read".to_owned(),
                        target: Some("README.md".to_owned()),
                        detail: None,
                    }),
                }],
            },
        };

        let value = serde_json::to_value(event).expect("serialize event");

        assert_eq!(
            value["kind"]["calls"][0],
            json!({
                "callId": "call_1",
                "toolName": "read_file",
                "argumentsRef": "sha256:args",
                "arguments": "{\"path\":\"README.md\"}",
                "display": {
                    "group": "explore",
                    "verb": "Read",
                    "target": "README.md"
                }
            })
        );
    }

    #[test]
    fn run_view_can_expose_tool_batches() {
        let run = RunView {
            id: "run_1".to_owned(),
            status: RunStatus::Running,
            input: Vec::new(),
            items: Vec::new(),
            tool_batches: vec![ToolBatchView {
                id: "tool_batch_1".to_owned(),
                turn_id: "turn_1".to_owned(),
                status: ToolItemStatus::Succeeded,
                calls: vec![ToolCallView {
                    call_id: "call_1".to_owned(),
                    tool_name: "read_file".to_owned(),
                    arguments_ref: "sha256:args".to_owned(),
                    arguments: Some(r#"{"path":"README.md"}"#.to_owned()),
                    output: Some("ok".to_owned()),
                    is_error: false,
                    status: ToolItemStatus::Succeeded,
                    display: Some(ToolCallDisplayView {
                        group: ToolCallDisplayGroup::Explore,
                        verb: "Read".to_owned(),
                        target: Some("README.md".to_owned()),
                        detail: None,
                    }),
                }],
            }],
        };

        let value = serde_json::to_value(run).expect("serialize run");

        assert_eq!(
            value["toolBatches"][0],
            json!({
                "id": "tool_batch_1",
                "turnId": "turn_1",
                "status": "succeeded",
                "calls": [{
                    "callId": "call_1",
                    "toolName": "read_file",
                    "argumentsRef": "sha256:args",
                    "arguments": "{\"path\":\"README.md\"}",
                    "output": "ok",
                    "isError": false,
                    "status": "succeeded",
                    "display": {
                        "group": "explore",
                        "verb": "Read",
                        "target": "README.md"
                    }
                }]
            })
        );
    }

    #[test]
    fn session_status_serializes_as_string_enum() {
        assert_eq!(
            serde_json::to_value(SessionStatus::Idle).expect("serialize status"),
            json!("idle")
        );
    }

    #[test]
    fn run_lifecycle_statuses_keep_cancelling_distinct() {
        assert_eq!(
            serde_json::to_value(RunStatus::Cancelling).expect("serialize status"),
            json!("cancelling")
        );
    }

    #[test]
    fn tool_call_status_can_represent_requested_calls() {
        assert_eq!(
            serde_json::to_value(ToolItemStatus::Requested).expect("serialize status"),
            json!("requested")
        );
    }
}
