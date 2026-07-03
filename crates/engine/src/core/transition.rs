//! Pure transition record for the session state machine.

use serde::{Deserialize, Serialize};

use crate::{CoreAgentEvent, CoreAgentEventKind, CoreAgentJoins, UncommittedCoreAgentEvent};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CoreAgentEventProposal {
    pub joins: CoreAgentJoins,
    pub kind: CoreAgentEventKind,
}

impl CoreAgentEventProposal {
    pub fn new(joins: CoreAgentJoins, kind: CoreAgentEventKind) -> Self {
        Self { joins, kind }
    }

    pub fn into_uncommitted(self, observed_at_ms: u64) -> UncommittedCoreAgentEvent {
        UncommittedCoreAgentEvent {
            observed_at_ms,
            joins: self.joins,
            event: CoreAgentEvent { kind: self.kind },
        }
    }
}
