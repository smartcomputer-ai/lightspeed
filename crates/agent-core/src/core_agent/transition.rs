//! Pure transition contracts for the session state machine.

use serde::{Deserialize, Serialize};

use crate::{
    CommandError, CoreAgentCommand, CoreAgentEntry, CoreAgentEvent, CoreAgentEventKind,
    CoreAgentJoins, CoreAgentState, DomainError, PlanningError, UncommittedCoreAgentEvent,
};

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

pub trait AdmitCommand: Send + Sync {
    fn admit(
        &self,
        state: &CoreAgentState,
        command: CoreAgentCommand,
    ) -> Result<Vec<CoreAgentEventProposal>, CommandError>;
}

pub trait ApplyEvent: Send + Sync {
    fn apply(&self, state: &mut CoreAgentState, entry: &CoreAgentEntry) -> Result<(), DomainError>;
}

pub trait PlanNext: Send + Sync {
    fn plan_next(
        &self,
        state: &CoreAgentState,
    ) -> Result<Vec<CoreAgentEventProposal>, PlanningError>;
}
