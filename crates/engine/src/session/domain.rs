use crate::session::{EventProposal, SessionEntry};

pub trait AgentDomain {
    type Command;
    type Event;
    type Joins;
    type State;
    type Error;

    fn initial_state(&self) -> Self::State;

    fn admit(
        &self,
        state: &Self::State,
        command: Self::Command,
    ) -> Result<Vec<EventProposal<Self::Event, Self::Joins>>, Self::Error>;

    fn apply(
        &self,
        state: &mut Self::State,
        entry: &SessionEntry<Self::Event, Self::Joins>,
    ) -> Result<(), Self::Error>;
}
