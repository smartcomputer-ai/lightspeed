//! Pure transition record for the session state machine.

use serde::{Deserialize, Serialize};

use crate::{CoreAgentEvent, CoreAgentJoins, UncommittedCoreAgentEvent};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CoreAgentEventProposal {
    pub joins: CoreAgentJoins,
    pub event: CoreAgentEvent,
}

impl CoreAgentEventProposal {
    pub fn new(joins: CoreAgentJoins, event: CoreAgentEvent) -> Self {
        Self { joins, event }
    }

    pub fn into_uncommitted(self, observed_at_ms: u64) -> UncommittedCoreAgentEvent {
        UncommittedCoreAgentEvent {
            observed_at_ms,
            joins: self.joins,
            event: self.event,
        }
    }
}
