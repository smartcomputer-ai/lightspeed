//! Codex-like built-in tool surface.
//!
//! This currently shares canonical Lightspeed schemas and argument decoding while
//! keeping a separate logical surface for dispatch and future Codex drift.

use serde_json::Value;

use crate::{error::ToolResult, runtime::ToolInvocationOutput, targets::ResolvedToolContext};

use super::{BuiltinToolOperation, canonical};

pub(super) fn description(operation: BuiltinToolOperation, scoped_paths: bool) -> String {
    canonical::description(operation, scoped_paths)
}

pub(super) fn input_schema(operation: BuiltinToolOperation) -> Value {
    canonical::input_schema(operation)
}

pub(super) async fn invoke_json(
    operation: BuiltinToolOperation,
    ctx: ResolvedToolContext<'_>,
    arguments: Value,
) -> ToolResult<ToolInvocationOutput> {
    canonical::invoke_json(operation, ctx, arguments).await
}
