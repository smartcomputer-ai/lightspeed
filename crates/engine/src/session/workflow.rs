use std::{error::Error, fmt};

use crate::{
    session::{
        AgentDomain, CodecError, DynamicSessionEntry, DynamicUncommittedSessionEvent, EventCodec,
        EventProposal, JoinsCodec, SessionEntry, SessionId, SessionPosition,
        UncommittedSessionEvent,
    },
    storage::{AppendSessionEvents, SessionStore, SessionStoreError},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppendAppliedEvents<E, J> {
    pub entries: Vec<SessionEntry<E, J>>,
    pub head: Option<SessionPosition>,
}

#[derive(Debug)]
pub enum SessionWorkflowError<E> {
    Domain(E),
    Codec(CodecError),
    Store(SessionStoreError),
}

impl<E> SessionWorkflowError<E> {
    pub fn domain(error: E) -> Self {
        Self::Domain(error)
    }
}

impl<E> From<CodecError> for SessionWorkflowError<E> {
    fn from(error: CodecError) -> Self {
        Self::Codec(error)
    }
}

impl<E> From<SessionStoreError> for SessionWorkflowError<E> {
    fn from(error: SessionStoreError) -> Self {
        Self::Store(error)
    }
}

impl<E: fmt::Display> fmt::Display for SessionWorkflowError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domain(error) => write!(f, "agent domain error: {error}"),
            Self::Codec(error) => write!(f, "{error}"),
            Self::Store(error) => write!(f, "{error}"),
        }
    }
}

impl<E> Error for SessionWorkflowError<E> where E: Error + 'static {}

pub fn apply_entry<D>(
    domain: &D,
    state: &mut D::State,
    entry: &SessionEntry<D::Event, D::Joins>,
) -> Result<(), D::Error>
where
    D: AgentDomain,
{
    domain.apply(state, entry)
}

pub fn apply_entries<D>(
    domain: &D,
    state: &mut D::State,
    entries: &[SessionEntry<D::Event, D::Joins>],
) -> Result<(), D::Error>
where
    D: AgentDomain,
{
    for entry in entries {
        apply_entry(domain, state, entry)?;
    }
    Ok(())
}

pub async fn append_admitted_command<D, C>(
    domain: &D,
    codec: &C,
    sessions: &dyn SessionStore,
    session_id: SessionId,
    expected_head: Option<SessionPosition>,
    state: &mut D::State,
    command: D::Command,
    observed_at_ms: u64,
) -> Result<AppendAppliedEvents<D::Event, D::Joins>, SessionWorkflowError<D::Error>>
where
    D: AgentDomain,
    C: EventCodec<Event = D::Event> + JoinsCodec<Joins = D::Joins>,
{
    let proposals = domain
        .admit(state, command)
        .map_err(SessionWorkflowError::Domain)?;
    append_event_proposals(
        domain,
        codec,
        sessions,
        session_id,
        expected_head,
        state,
        proposals,
        observed_at_ms,
    )
    .await
}

pub async fn append_event_proposals<D, C>(
    domain: &D,
    codec: &C,
    sessions: &dyn SessionStore,
    session_id: SessionId,
    expected_head: Option<SessionPosition>,
    state: &mut D::State,
    proposals: Vec<EventProposal<D::Event, D::Joins>>,
    observed_at_ms: u64,
) -> Result<AppendAppliedEvents<D::Event, D::Joins>, SessionWorkflowError<D::Error>>
where
    D: AgentDomain,
    C: EventCodec<Event = D::Event> + JoinsCodec<Joins = D::Joins>,
{
    let events = proposals
        .into_iter()
        .map(|proposal| proposal.into_uncommitted(observed_at_ms))
        .map(|event| encode_uncommitted_event(codec, &event))
        .collect::<Result<Vec<_>, _>>()?;

    let appended = sessions
        .append(AppendSessionEvents {
            session_id,
            expected_head,
            events,
        })
        .await?;

    let entries = appended
        .entries
        .iter()
        .map(|entry| decode_session_entry(codec, entry))
        .collect::<Result<Vec<_>, _>>()?;
    apply_entries(domain, state, &entries).map_err(SessionWorkflowError::Domain)?;

    Ok(AppendAppliedEvents {
        entries,
        head: appended.head,
    })
}

pub fn encode_uncommitted_event<E, J, C>(
    codec: &C,
    event: &UncommittedSessionEvent<E, J>,
) -> Result<DynamicUncommittedSessionEvent, CodecError>
where
    C: EventCodec<Event = E> + JoinsCodec<Joins = J>,
{
    Ok(DynamicUncommittedSessionEvent {
        observed_at_ms: event.observed_at_ms,
        joins: codec.encode_joins(&event.joins),
        event: codec.encode_event(&event.event)?,
    })
}

pub fn decode_session_entry<E, J, C>(
    codec: &C,
    entry: &DynamicSessionEntry,
) -> Result<SessionEntry<E, J>, CodecError>
where
    C: EventCodec<Event = E> + JoinsCodec<Joins = J>,
{
    Ok(SessionEntry {
        position: entry.position.clone(),
        observed_at_ms: entry.observed_at_ms,
        joins: codec.decode_joins(&entry.joins)?,
        event: codec.decode_event(&entry.event)?,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        session::{AgentHandle, DynamicEvent, DynamicJoins},
        storage::{CreateSession, InMemorySessionStore},
    };

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Command {
        Add(u32),
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Event {
        Added(u32),
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    struct Joins;

    struct CounterDomain;

    impl AgentDomain for CounterDomain {
        type Command = Command;
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
            command: Self::Command,
        ) -> Result<Vec<EventProposal<Self::Event, Self::Joins>>, Self::Error> {
            match command {
                Command::Add(value) => Ok(vec![EventProposal::new(Joins, Event::Added(value))]),
            }
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

    struct CounterCodec;

    impl EventCodec for CounterCodec {
        type Event = Event;

        fn encode_event(&self, event: &Self::Event) -> Result<DynamicEvent, CodecError> {
            match event {
                Event::Added(value) => Ok(DynamicEvent::new(
                    "test.counter.added",
                    1,
                    json!({ "value": value }),
                )),
            }
        }

        fn decode_event(&self, event: &DynamicEvent) -> Result<Self::Event, CodecError> {
            if event.kind != "test.counter.added" || event.version != 1 {
                return Err(CodecError::Unsupported {
                    kind: event.kind.clone(),
                    version: event.version,
                });
            }
            let value = event
                .payload
                .get("value")
                .and_then(|value| value.as_u64())
                .ok_or_else(|| CodecError::Failed {
                    message: "missing value".to_owned(),
                })?;
            Ok(Event::Added(value as u32))
        }
    }

    impl JoinsCodec for CounterCodec {
        type Joins = Joins;

        fn encode_joins(&self, _joins: &Self::Joins) -> DynamicJoins {
            DynamicJoins::new()
        }

        fn decode_joins(&self, _joins: &DynamicJoins) -> Result<Self::Joins, CodecError> {
            Ok(Joins)
        }
    }

    #[tokio::test]
    async fn append_admitted_command_commits_and_applies_entries() {
        let store = InMemorySessionStore::new();
        let session_id = SessionId::new("session_1");
        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("test_agent"),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        let domain = CounterDomain;
        let codec = CounterCodec;
        let mut state = domain.initial_state();

        let result = append_admitted_command(
            &domain,
            &codec,
            &store,
            session_id,
            None,
            &mut state,
            Command::Add(3),
            10,
        )
        .await
        .expect("append command");

        assert_eq!(state, 3);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].position.seq.as_u64(), 1);
        assert_eq!(result.head.expect("head").seq.as_u64(), 1);
    }
}
