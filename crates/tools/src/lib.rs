//! Optional standard agent tools for `harness`.
//!
//! This crate owns optional tool packages, model-visible tool contracts,
//! and protocol/runtime adapters. The deterministic `harness` core stays
//! independent from this crate.

pub mod attachments;
pub mod builtin;
pub mod callable;
pub mod catalog;
pub mod code;
pub mod concurrency;
pub mod definitions;
pub mod environment;
pub mod environment_protocol;
pub mod error;
pub mod fs;
pub mod limits;
pub mod prompts;
pub mod runtime;
pub mod skills;
mod subagent_catalog_text;
pub mod subagents;
pub mod toolset;
pub mod transfer;
pub mod web;
pub mod workflow_tool;

pub use error::{ToolError, ToolResult};
