use std::{future::Future, time::Duration};

use codemode::{Execution, ExecutionEvent, ExecutionInput, ExecutionLimits, ToolBinding};

/// Bound the whole host scenario independently of interpreter deadline checks.
pub async fn bounded<T>(scenario: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), scenario)
        .await
        .expect("code host scenario exceeded its outer deadline")
}

pub fn input(source: &str, bindings: &[(&str, &str)]) -> ExecutionInput {
    ExecutionInput {
        source: source.into(),
        bindings: bindings
            .iter()
            .map(|(name, id)| ToolBinding {
                name: (*name).into(),
                binding_id: (*id).into(),
            })
            .collect(),
        limits: ExecutionLimits {
            timeout_ms: 5_000,
            max_memory_bytes: 16 * 1024 * 1024,
            max_stack_bytes: 256 * 1024,
            max_source_bytes: 64 * 1024,
            max_catalog_bytes: 64 * 1024,
            max_request_bytes: 64 * 1024,
            max_result_bytes: 64 * 1024,
            max_output_bytes: 64 * 1024,
            max_tool_calls: 64,
            max_outstanding_tool_calls: 8,
        },
    }
}

pub async fn event(execution: &mut Execution) -> ExecutionEvent {
    execution
        .next_event()
        .await
        .expect("missing terminal report")
}
