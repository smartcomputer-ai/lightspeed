use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::session::{EventSeq, StoredEvent};

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

pub type StoredJoins = BTreeMap<String, String>;

pub type StoredSessionEntry = SessionEntry<StoredEvent, StoredJoins>;

pub type UncommittedStoredEvent = UncommittedSessionEvent<StoredEvent, StoredJoins>;
