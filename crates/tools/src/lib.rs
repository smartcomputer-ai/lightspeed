//! Optional standard agent tools for `engine`.
//!
//! This crate owns optional tool packages, model-visible tool contracts,
//! and host/runtime adapters. The deterministic `engine` core stays
//! independent from this crate.

pub mod error;
pub mod host;
pub mod runtime;
pub mod skills;

pub use error::{ToolError, ToolResult};
