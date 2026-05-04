use llm_clients::ProviderFailureKind;
use llm_clients::anthropic::messages::{
    API_KIND, Client, Config, CountTokensRequest, CreateMessageRequest, StopReason, Thinking, Tool,
    ToolChoice, ToolDefinition,
};
use serde_json::json;
use std::path::PathBuf;

fn live_model() -> String {
    env_or_dotenv_var("ANTHROPIC_MESSAGES_MODEL")
        .or_else(|_| env_or_dotenv_var("ANTHROPIC_LIVE_MODEL"))
        .unwrap_or_else(|_| "claude-sonnet-4-5".to_string())
}

fn live_client() -> Client {
    let api_key = env_or_dotenv_var("ANTHROPIC_API_KEY").expect(
        "ANTHROPIC_API_KEY must be set in env or root .env to run anthropic:messages live tests",
    );
    assert!(
        !api_key.trim().is_empty(),
        "ANTHROPIC_API_KEY is set but empty"
    );

    let mut config = Config::new(api_key);
    if let Ok(base_url) = env_or_dotenv_var("ANTHROPIC_BASE_URL") {
        config.base_url = base_url;
    }
    if let Ok(version) = env_or_dotenv_var("ANTHROPIC_VERSION") {
        config.anthropic_version = version;
    }
    if let Ok(beta_headers) = env_or_dotenv_var("ANTHROPIC_BETA") {
        config.beta_headers = beta_headers
            .split([',', ' '])
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect();
    }

    Client::new(config).expect("Anthropic Messages client")
}

fn env_or_dotenv_var(name: &str) -> Result<String, std::env::VarError> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(env_error) => dotenv_var(name).ok_or(env_error),
    }
}

fn dotenv_var(name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(root_dotenv_path()).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        if key.trim() == name {
            return Some(unquote_dotenv_value(value.trim()));
        }
    }
    None
}

fn root_dotenv_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .join(".env")
}

fn unquote_dotenv_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_create_text() {
    let client = live_client();
    let request = CreateMessageRequest::user_text(
        live_model(),
        "Reply with exactly these two words: library messages",
        64,
    );

    let response = client.create(request).await.expect("create message");

    assert_eq!(response.status, 200);
    assert!(!response.parsed.id.is_empty());
    assert_eq!(response.parsed.stop_reason, Some(StopReason::EndTurn));
    assert!(
        response
            .parsed
            .output_text()
            .to_lowercase()
            .contains("library"),
        "expected visible text output, got {:?}",
        response.parsed.content
    );
    assert!(
        response
            .parsed
            .usage
            .as_ref()
            .and_then(|usage| usage.total_tokens())
            .unwrap_or_default()
            > 0,
        "expected usage tokens"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_stream_text() {
    let client = live_client();
    let request =
        CreateMessageRequest::user_text(live_model(), "Reply with exactly: streaming ok", 64);
    let mut stream = client.stream(request).await.expect("stream message");

    let mut saw_delta = false;
    let mut saw_terminal = false;
    while let Some(event) = stream.next_event().await.expect("stream event") {
        if event.parsed.text_delta().is_some() {
            saw_delta = true;
        }
        if event.parsed.is_terminal() {
            saw_terminal = true;
            break;
        }
    }

    assert!(saw_delta, "expected at least one text delta");
    assert!(saw_terminal, "expected terminal stream event");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_forced_tool_use() {
    let client = live_client();
    let mut tool = ToolDefinition::new(
        "get_weather",
        json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"]
        }),
    );
    tool.description = Some("Get the current weather for a city".to_string());

    let mut request = CreateMessageRequest::user_text(
        live_model(),
        "Call get_weather for Zurich. Do not answer in natural language.",
        256,
    );
    request.tools = Some(vec![Tool::Custom(tool)]);
    request.tool_choice = Some(ToolChoice::tool("get_weather"));

    let response = client.create(request).await.expect("tool use message");
    let tools = response.parsed.tool_uses().collect::<Vec<_>>();

    assert_eq!(response.parsed.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(tools.len(), 1, "expected one forced tool use");
    assert_eq!(tools[0].name, "get_weather");
    assert!(
        tools[0]
            .input
            .and_then(|input| input.get("city"))
            .and_then(|city| city.as_str())
            .unwrap_or_default()
            .contains("Zurich"),
        "expected Zurich in tool input: {:?}",
        tools[0].input
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_count_tokens() {
    let client = live_client();
    let request = CountTokensRequest::user_text(live_model(), "Count these input tokens.");

    let tokens = client
        .count_tokens(request)
        .await
        .expect("count message tokens");

    assert_eq!(tokens.status, 200);
    assert!(
        tokens.parsed.input_tokens.unwrap_or_default() > 0,
        "expected input token count"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_thinking() {
    let client = live_client();
    let mut request = CreateMessageRequest::user_text(
        live_model(),
        "What is 17 + 25? Answer with the final number.",
        1200,
    );
    request.thinking = Some(Thinking::enabled(1024));

    let response = client.create(request).await.expect("thinking message");

    assert!(
        response.parsed.thinking_blocks().next().is_some(),
        "expected a thinking block, got {:?}",
        response.parsed.content
    );
    assert!(
        response.parsed.output_text().contains("42"),
        "expected final answer text to contain 42, got {:?}",
        response.parsed.content
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ANTHROPIC_API_KEY (costs real money)"]
async fn anthropic_messages_live_invalid_model_classifies_provider_error() {
    let client = live_client();
    let request = CreateMessageRequest::user_text(
        "claude-this-model-should-not-exist-live-test",
        "hello",
        64,
    );

    let error = client
        .create(request)
        .await
        .expect_err("invalid model should fail");

    match error {
        llm_clients::LlmApiError::HttpStatus(provider) => {
            assert_eq!(provider.api_kind, API_KIND);
            assert!(
                matches!(
                    provider.kind,
                    ProviderFailureKind::InvalidRequest
                        | ProviderFailureKind::NotFound
                        | ProviderFailureKind::Other
                ),
                "unexpected provider failure kind: {:?}",
                provider.kind
            );
            assert!(provider.raw_json.is_some() || provider.raw_text.is_some());
        }
        other => panic!("expected provider HTTP error, got {other:?}"),
    }
}
