//! Optional standard agent tools for `agent-core`.
//!
//! This crate owns optional tool packages, model-visible tool contracts,
//! and host/runtime adapters. The deterministic `agent-core` core stays
//! independent from this crate.

pub mod error;
pub mod host;
pub mod runtime;

pub use error::{ToolError, ToolResult};
