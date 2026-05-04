//! Protocol types for host execution targets.
//!
//! This crate is intentionally transport-free. It defines the stable
//! request/response records used by clients, controllers, and host
//! implementations.

pub mod control;
pub mod data;
pub mod error;
pub mod shared;
