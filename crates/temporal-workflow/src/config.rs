use std::time::Duration;

use engine::{
    AnthropicMessagesRequestDefaults, ContextConfig, ModelSelection,
    OpenAiCompletionsRequestDefaults, OpenAiResponsesRequestDefaults, ProviderApiKind,
    ProviderRequestDefaults, RunConfig, SessionConfig, TurnConfig,
};
use temporalio_sdk::ActivityOptions;

pub const DEFAULT_TASK_QUEUE: &str = "forge-agent";
pub const DEFAULT_TEMPORAL_TARGET: &str = "http://localhost:7233";
pub const DEFAULT_TEMPORAL_NAMESPACE: &str = "default";
pub const DEFAULT_MODEL: &str = "gpt-5.5";
pub const DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD: u32 = 10_000;
pub const DEFAULT_ACTIVITY_START_TO_CLOSE_TIMEOUT: Duration = Duration::from_secs(360);

pub const FAKE_TOOL_NAME: &str = "agent_echo";

pub fn default_run_config() -> RunConfig {
    RunConfig {
        max_turns: None,
        max_tool_rounds: None,
        model_override: None,
        max_output_tokens: None,
        provider_request_defaults: None,
    }
}

pub fn default_session_config(model: ModelSelection) -> SessionConfig {
    let provider_request_defaults = default_provider_request_defaults(&model.api_kind);
    SessionConfig {
        model,
        run: default_run_config(),
        turn: TurnConfig {
            max_output_tokens: None,
            provider_request_defaults,
        },
        context: ContextConfig { compaction: None },
        tools: engine::ToolConfig::default(),
    }
}

fn default_provider_request_defaults(api_kind: &ProviderApiKind) -> ProviderRequestDefaults {
    match api_kind {
        ProviderApiKind::OpenAiResponses => {
            ProviderRequestDefaults::OpenAiResponses(OpenAiResponsesRequestDefaults::default())
        }
        ProviderApiKind::AnthropicMessages => {
            ProviderRequestDefaults::AnthropicMessages(AnthropicMessagesRequestDefaults::default())
        }
        ProviderApiKind::OpenAiCompletions => {
            ProviderRequestDefaults::OpenAiCompletions(OpenAiCompletionsRequestDefaults::default())
        }
    }
}

pub fn default_instructions() -> &'static str {
    "You are Forge, a concise personal assistant. Use available tools when useful, then answer plainly."
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
