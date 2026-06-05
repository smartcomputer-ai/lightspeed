use std::{collections::BTreeMap, time::Duration};

use engine::{
    AnthropicMessagesRequestDefaults, BlobRef, ContextConfig, FunctionToolSpec, ModelSelection,
    OpenAiCompletionsRequestDefaults, OpenAiResponsesRequestDefaults, ProviderApiKind,
    ProviderRequestDefaults, RunConfig, SessionConfig, ToolChoice, ToolChoiceMode, ToolKind,
    ToolName, ToolParallelism, ToolProfile, ToolProfileId, ToolRegistry, ToolSpec,
    ToolTargetRequirement, TurnConfig,
};
use temporalio_sdk::ActivityOptions;

pub const DEFAULT_TASK_QUEUE: &str = "forge-agent";
pub const DEFAULT_TEMPORAL_TARGET: &str = "http://localhost:7233";
pub const DEFAULT_TEMPORAL_NAMESPACE: &str = "default";
pub const DEFAULT_MODEL: &str = "gpt-5.5";
pub const DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD: u32 = 10_000;

pub const FAKE_TOOL_PROFILE_ID: &str = "agent_fake_tools";
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

pub fn fake_tool_input_schema() -> Vec<u8> {
    br#"{"type":"object","additionalProperties":false,"properties":{"text":{"type":"string"}},"required":["text"]}"#.to_vec()
}

pub fn fake_tool_registry(input_schema_ref: BlobRef) -> ToolRegistry {
    let tool_name = ToolName::new(FAKE_TOOL_NAME);
    let profile_id = ToolProfileId::new(FAKE_TOOL_PROFILE_ID);
    ToolRegistry {
        tools: BTreeMap::from([(
            tool_name.clone(),
            ToolSpec {
                name: tool_name.clone(),
                kind: ToolKind::Function(FunctionToolSpec {
                    model_name: None,
                    description_ref: None,
                    input_schema_ref,
                    output_schema_ref: None,
                    strict: Some(true),
                    provider_options_ref: None,
                }),
                parallelism: ToolParallelism::ParallelSafe,
                target_requirement: ToolTargetRequirement::None,
            },
        )]),
        profiles: BTreeMap::from([(
            profile_id.clone(),
            ToolProfile {
                profile_id,
                visible_tools: vec![tool_name.clone()],
                tool_choice: Some(ToolChoice {
                    mode: ToolChoiceMode::Auto,
                    disable_parallel_tool_use: Some(true),
                }),
            },
        )]),
    }
}

pub fn activity_options() -> ActivityOptions {
    ActivityOptions::start_to_close_timeout(Duration::from_secs(300))
}
