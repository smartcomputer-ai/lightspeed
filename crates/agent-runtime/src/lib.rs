//! Local runtime composition helpers for Forge-native agents.
//!
//! The deterministic `agent-core` crate owns session state and runner planning.
//! Adapter crates own concrete LLM and tool execution. This crate wires those
//! pieces together for inline/local SDK use without becoming a provider or tool
//! implementation itself.

pub mod api;

mod projection;
