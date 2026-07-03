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

pub type DynamicJoins = BTreeMap<String, String>;

pub type DynamicSessionEntry = SessionEntry<DynamicEvent, DynamicJoins>;

pub type DynamicUncommittedSessionEvent = UncommittedSessionEvent<DynamicEvent, DynamicJoins>;
