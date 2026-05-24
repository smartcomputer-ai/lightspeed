use thiserror::Error;

use crate::{
    AdmitCommand, AgentDomain, ApplyEvent, CommandError, CoreAdmitCommand, CoreAgentCommand,
    CoreAgentEntry, CoreAgentEvent, CoreAgentJoins, CoreAgentState, CoreApplyEvent, DomainError,
    EventProposal,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreAgentDomain {
    admit: CoreAdmitCommand,
    apply: CoreApplyEvent,
}

impl CoreAgentDomain {
    pub fn new() -> Self {
        Self::default()
    }
}

impl AgentDomain for CoreAgentDomain {
    type Command = CoreAgentCommand;
    type Event = CoreAgentEvent;
    type Joins = CoreAgentJoins;
    type State = CoreAgentState;
    type Error = CoreAgentDomainError;

    fn initial_state(&self) -> Self::State {
        CoreAgentState::new()
    }

    fn admit(
        &self,
        state: &Self::State,
        command: Self::Command,
    ) -> Result<Vec<EventProposal<Self::Event, Self::Joins>>, Self::Error> {
        self.admit
            .admit(state, command)?
            .into_iter()
            .map(|proposal| {
                Ok(EventProposal::new(
                    proposal.joins,
                    CoreAgentEvent {
                        kind: proposal.kind,
                    },
                ))
            })
            .collect()
    }

    fn apply(&self, state: &mut Self::State, entry: &CoreAgentEntry) -> Result<(), Self::Error> {
        self.apply.apply(state, entry)?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum CoreAgentDomainError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Domain(#[from] DomainError),
}
