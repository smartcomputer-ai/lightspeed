//! Session event-log primitives.
//!
//! This module owns the durable session-log vocabulary: ids, log entry
//! shapes, and the stored event envelope. The CoreAgent domain in `core` is
//! the only producer and consumer of these entries; there is no pluggable
//! agent domain.

pub mod codec;
pub mod dynamic;
pub mod ids;
pub mod log;

pub use codec::*;
pub use dynamic::*;
pub use ids::*;
pub use log::*;
