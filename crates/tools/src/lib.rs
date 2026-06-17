//! Optional standard agent tools for `engine`.
//!
//! This crate owns optional tool packages, model-visible tool contracts,
//! and protocol/runtime adapters. The deterministic `engine` core stays
//! independent from this crate.

pub mod builtin;
pub mod environment;
pub mod error;
pub mod fs;
pub mod host_protocol;
pub mod limits;
pub mod messaging;
pub mod prompts;
pub mod runtime;
pub mod skills;
pub mod targets;
pub mod toolset;
pub mod web;

pub use error::{ToolError, ToolResult};
