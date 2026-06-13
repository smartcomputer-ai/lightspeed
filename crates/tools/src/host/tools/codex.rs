//! Codex-like host tool surface.
//!
//! This currently shares canonical Lightspeed schemas and argument decoding while
//! keeping a separate logical surface for dispatch and future Codex drift.

use serde_json::Value;

use crate::{error::ToolResult, host::context::HostToolContext, runtime::ToolInvocationOutput};

use super::{HostToolOperation, canonical};

pub(super) fn description(operation: HostToolOperation, scoped_paths: bool) -> String {
    canonical::description(operation, scoped_paths)
}

pub(super) fn input_schema(operation: HostToolOperation) -> Value {
    canonical::input_schema(operation)
}

pub(super) async fn invoke_json(
    operation: HostToolOperation,
    ctx: &HostToolContext,
    arguments: Value,
) -> ToolResult<ToolInvocationOutput> {
    canonical::invoke_json(operation, ctx, arguments).await
}
