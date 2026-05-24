use crate::session::{AgentDomain, SessionEntry};

pub fn replay<D>(
    domain: &D,
    entries: &[SessionEntry<D::Event, D::Joins>],
) -> Result<D::State, D::Error>
where
    D: AgentDomain,
{
    let mut state = domain.initial_state();
    for entry in entries {
        domain.apply(&mut state, entry)?;
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{EventProposal, EventSeq, SessionPosition};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Event {
        Added(u32),
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    struct Joins;

    struct CounterDomain;

    impl AgentDomain for CounterDomain {
        type Command = ();
        type Event = Event;
        type Joins = Joins;
        type State = u32;
        type Error = String;

        fn initial_state(&self) -> Self::State {
            0
        }

        fn admit(
            &self,
            _state: &Self::State,
            _command: Self::Command,
        ) -> Result<Vec<EventProposal<Self::Event, Self::Joins>>, Self::Error> {
            Ok(Vec::new())
        }

        fn apply(
            &self,
            state: &mut Self::State,
            entry: &SessionEntry<Self::Event, Self::Joins>,
        ) -> Result<(), Self::Error> {
            match entry.event {
                Event::Added(value) => *state += value,
            }
            Ok(())
        }
    }

    #[test]
    fn replay_applies_entries_in_order() {
        let entries = vec![entry(1, Event::Added(2)), entry(2, Event::Added(3))];

        assert_eq!(replay(&CounterDomain, &entries), Ok(5));
    }

    fn entry(seq: u64, event: Event) -> SessionEntry<Event, Joins> {
        SessionEntry {
            position: SessionPosition {
                seq: EventSeq::new(seq),
            },
            observed_at_ms: seq,
            joins: Joins,
            event,
        }
    }
}
