//! Compatibility exports for older CoreAgent workflow helper paths.
//!
//! CoreAgent driving now lives in `core::drive` as a substrate-neutral
//! action machine. This module intentionally contains no async runtime or
//! storage execution logic.

pub use super::drive::{
    CoreAgentAction, CoreAgentDrive, CoreAgentDriveError, classify_core_agent_action,
    generation_result_proposals, next_generation_request, next_tool_batch_request,
    tool_batch_result_proposals,
};
