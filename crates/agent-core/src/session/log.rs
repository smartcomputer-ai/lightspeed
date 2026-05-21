use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::session::{DynamicEvent, EventSeq};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPosition {
    pub seq: EventSeq,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntry<E, J = ()> {
    pub position: SessionPosition,
    pub observed_at_ms: u64,
    pub joins: J,
    pub event: E,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UncommittedSessionEvent<E, J = ()> {
    pub observed_at_ms: u64,
    pub joins: J,
    pub event: E,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventProposal<E, J = ()> {
    pub joins: J,
    pub event: E,
}

pub type DynamicJoins = BTreeMap<String, String>;

pub type DynamicSessionEntry = SessionEntry<DynamicEvent, DynamicJoins>;

pub type DynamicUncommittedSessionEvent = UncommittedSessionEvent<DynamicEvent, DynamicJoins>;

impl<E, J> EventProposal<E, J> {
    pub fn new(joins: J, event: E) -> Self {
        Self { joins, event }
    }

    pub fn into_uncommitted(self, observed_at_ms: u64) -> UncommittedSessionEvent<E, J> {
        UncommittedSessionEvent {
            observed_at_ms,
            joins: self.joins,
            event: self.event,
        }
    }
}
