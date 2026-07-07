use std::time::Duration;

use engine::{ContextConfig, ModelSelection, RunConfig, SessionConfig, TurnConfig};
use temporalio_sdk::ActivityOptions;

pub const DEFAULT_TASK_QUEUE: &str = "lightspeed-agent";
pub const DEFAULT_TEMPORAL_TARGET: &str = "http://localhost:7233";
pub const DEFAULT_TEMPORAL_NAMESPACE: &str = "default";
pub const DEFAULT_MODEL: &str = "gpt-5.5";
pub const DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD: u32 = 10_000;
pub const DEFAULT_ACTIVITY_START_TO_CLOSE_TIMEOUT: Duration = Duration::from_secs(360);

/// Conservative ceiling on the serialized compact bootstrap result (replayed
/// `CoreAgentState` plus small indices). Temporal's default activity-result
/// payload limit is 2 MiB; we guard well below it so a near-limit reduced state
/// fails with a typed `SessionBootstrapPayloadTooLarge` error before Temporal
/// rejects the activity completion with an opaque size error. Reduced state is
/// bounded by active context (entry metadata + content refs), not by total log
/// length, so this budget should never be hit in normal operation.
pub const DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES: u64 = 1_500_000;

pub const FAKE_TOOL_NAME: &str = "agent_echo";

pub fn default_run_config() -> RunConfig {
    RunConfig {
        max_turns: None,
        max_tool_rounds: None,
        model_override: None,
        max_output_tokens: None,
        provider_params: None,
        tool_choice: None,
    }
}

pub fn default_session_config(model: ModelSelection) -> SessionConfig {
    SessionConfig {
        model,
        run: default_run_config(),
        turn: TurnConfig {
            max_output_tokens: None,
            tool_choice: None,
            provider_params: None,
        },
        context: ContextConfig { compaction: None },
        tools: engine::ToolConfig::default(),
        fleet: engine::FleetConfig::default(),
    }
}

pub fn default_instructions() -> &'static str {
    "You are Lightspeed, a concise personal assistant. Use available tools when useful, then answer plainly."
}

pub fn activity_options() -> ActivityOptions {
    ActivityOptions::start_to_close_timeout(DEFAULT_ACTIVITY_START_TO_CLOSE_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use temporalio_sdk::ActivityCloseTimeouts;

    #[test]
    fn activity_options_use_extended_start_to_close_timeout() {
        assert_eq!(
            activity_options().close_timeouts,
            ActivityCloseTimeouts::StartToClose(DEFAULT_ACTIVITY_START_TO_CLOSE_TIMEOUT)
        );
    }
}
