use crate::session::{DynamicCommand, DynamicEvent, DynamicJoins};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CodecError {
    #[error("unsupported envelope kind {kind:?} version {version}")]
    Unsupported { kind: String, version: u32 },

    #[error("codec failure: {message}")]
    Failed { message: String },
}

pub trait CommandCodec {
    type Command;

    fn encode_command(&self, command: &Self::Command) -> Result<DynamicCommand, CodecError>;
    fn decode_command(&self, command: &DynamicCommand) -> Result<Self::Command, CodecError>;
}

pub trait EventCodec {
    type Event;

    fn encode_event(&self, event: &Self::Event) -> Result<DynamicEvent, CodecError>;
    fn decode_event(&self, event: &DynamicEvent) -> Result<Self::Event, CodecError>;
}

pub trait JoinsCodec {
    type Joins;

    fn encode_joins(&self, joins: &Self::Joins) -> DynamicJoins;
    fn decode_joins(&self, joins: &DynamicJoins) -> Result<Self::Joins, CodecError>;
}
