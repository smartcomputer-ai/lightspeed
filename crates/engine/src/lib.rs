//! Lightspeed-native agent core contracts.
//!
//! This crate defines the session-log primitives plus the closed CoreAgent
//! domain: command/event/state vocabulary, provider-native LLM/tool request
//! records, logical storage traits, deterministic planning, and a
//! substrate-neutral CoreAgent drive machine. It does not execute provider
//! calls, runtime tools, process runners, Temporal workflows, or production
//! persistence.

pub mod blob;
pub mod core;
pub mod session;
pub mod storage;

pub use blob::*;
pub use core::*;
pub use session::{CodecError, StoredEvent};
