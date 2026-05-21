use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{
    CommandRejection, CoreAgentCommand, CoreAgentEntry, CoreAgentState, SessionId, SessionPosition,
    storage::{BlobStore, SessionStore},
};

pub const DEFAULT_MAX_STEPS: u32 = 128;

#[derive(Clone)]
pub struct RunnerStores {
    pub sessions: Arc<dyn SessionStore>,
    pub blobs: Arc<dyn BlobStore>,
}

impl RunnerStores {
    pub fn new(sessions: Arc<dyn SessionStore>, blobs: Arc<dyn BlobStore>) -> Self {
        Self { sessions, blobs }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveCommand {
    pub session_id: SessionId,
    pub observed_at_ms: u64,
    pub command: CoreAgentCommand,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveSession {
    pub session_id: SessionId,
    pub observed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveOutcome {
    pub session_id: SessionId,
    pub accepted: bool,
    pub rejection: Option<CommandRejection>,
    pub head: Option<SessionPosition>,
    pub emitted_entries: Vec<CoreAgentEntry>,
    pub state: CoreAgentState,
    pub quiescence: RunnerQuiescence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerQuiescence {
    Idle,
    Closed,
    IterationLimitReached,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn drive_session_defaults_missing_max_steps_to_none() {
        let request: DriveSession = serde_json::from_value(json!({
            "session_id": "session-a",
            "observed_at_ms": 10
        }))
        .expect("deserialize drive session");

        assert_eq!(request.max_steps, None);
    }

    #[test]
    fn drive_session_omits_none_max_steps() {
        let request = DriveSession {
            session_id: SessionId::new("session-a"),
            observed_at_ms: 10,
            max_steps: None,
        };

        let value = serde_json::to_value(request).expect("serialize drive session");

        assert_eq!(
            value,
            json!({
                "session_id": "session-a",
                "observed_at_ms": 10
            })
        );
    }
}
