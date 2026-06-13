//! Lightspeed-native agent core contracts.
//!
//! This crate defines extensible session-log primitives plus the built-in
//! CoreAgent domain: command/event/state vocabulary, provider-native LLM/tool
//! request records, logical storage traits, deterministic planning contracts,
//! and a substrate-neutral CoreAgent drive machine. It does not execute
//! provider calls, host tools, process runners, Temporal workflows, or
//! production persistence.

pub mod blob;
pub mod core;
pub mod session;
pub mod storage;

pub use blob::*;
pub use core::*;
pub use session::{
    AgentDomain, AppendAppliedEvents, CodecError, CommandCodec, DynamicCommand, DynamicEvent,
    EventCodec, EventProposal, JoinsCodec, SessionWorkflowError, append_admitted_command,
    append_event_proposals, apply_entries, apply_entry, decode_session_entry,
    encode_uncommitted_event, replay,
};
