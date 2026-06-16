use llm_clients::ProviderFailureKind;
use llm_clients::openai::completions::{
    API_KIND, Client, CompletionTool, CompletionToolChoice, CompletionToolChoiceFunction,
    CompletionToolType, Config, CreateCompletionRequest,
};
use serde_json::json;

mod support;

use support::{
    env_or_dotenv_var, openai_completions_create, openai_completions_stream,
    required_first_env_or_dotenv_var,
};

fn live_model() -> String {
    env_or_dotenv_var("OPENAI_COMPLETIONS_MODEL")
        .or_else(|_| env_or_dotenv_var("OPENAI_LIVE_MODEL"))
        .unwrap_or_else(|_| "gpt-5.5".to_string())
}

fn live_client() -> Client {
    let api_key = required_first_env_or_dotenv_var(
        &["OPENAI_COMPLETIONS_API_KEY", "OPENAI_API_KEY"],
        "OPENAI_COMPLETIONS_API_KEY or OPENAI_API_KEY must be set in env or root .env to run openai:completions live tests",
    );

    let mut config = Config::new(api_key);
    if let Ok(base_url) = env_or_dotenv_var("OPENAI_COMPLETIONS_BASE_URL")
        .or_else(|_| env_or_dotenv_var("OPENAI_BASE_URL"))
    {
        config.base_url = base_url;
    }
    if let Ok(org_id) = env_or_dotenv_var("OPENAI_ORG_ID") {
        config.organization = Some(org_id);
    }
    if let Ok(project) = env_or_dotenv_var("OPENAI_PROJECT_ID") {
        config.project = Some(project);
    }

    Client::new(config).expect("OpenAI completions client")
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY or compatible endpoint credentials (costs real money)"]
async fn openai_completions_live_create_text() {
    let client = live_client();
    let request = CreateCompletionRequest::user_text(
        live_model(),
        "Reply with exactly these two words: completion transport",
    );

    let response = openai_completions_create(&client, request)
        .await
        .expect("create completion");

    assert_eq!(response.status, 200);
    assert!(!response.parsed.id.is_empty());
    assert!(
        response
            .parsed
            .output_text()
            .to_lowercase()
            .contains("completion"),
        "expected visible text output, got {:?}",
        response.parsed.choices
    );
    assert!(
        response
            .parsed
            .usage
            .as_ref()
            .and_then(|usage| usage.total_tokens)
            .unwrap_or_default()
            > 0,
        "expected usage tokens"
    );
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY or compatible endpoint credentials (costs real money)"]
async fn openai_completions_live_stream_text() {
    let client = live_client();
    let request =
        CreateCompletionRequest::user_text(live_model(), "Reply with exactly: completion stream");
    let mut stream = openai_completions_stream(&client, request)
        .await
        .expect("stream completion");

    let mut saw_delta = false;
    let mut saw_terminal = false;
    while let Some(event) = stream.next_chunk().await.expect("stream chunk") {
        if !event.parsed.text_delta().is_empty() {
            saw_delta = true;
        }
        if event.parsed.is_terminal() {
            saw_terminal = true;
            break;
        }
    }

    assert!(saw_delta, "expected at least one text delta");
    assert!(saw_terminal, "expected terminal stream chunk");
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY or compatible endpoint credentials (costs real money)"]
async fn openai_completions_live_forced_function_call() {
    let client = live_client();
    let mut tool = CompletionTool::function(
        "get_weather",
        json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"],
            "additionalProperties": false
        }),
    );
    tool.function.description = Some("Get the current weather for a city".to_string());
    tool.function.strict = Some(true);

    let mut request = CreateCompletionRequest::user_text(
        live_model(),
        "Call get_weather for Zurich. Do not answer in natural language.",
    );
    request.tools = Some(vec![tool]);
    request.tool_choice = Some(CompletionToolChoice::Function {
        r#type: CompletionToolType::Function,
        function: CompletionToolChoiceFunction {
            name: "get_weather".to_string(),
        },
    });

    let response = openai_completions_create(&client, request)
        .await
        .expect("function call completion");
    let calls = response.parsed.tool_calls().collect::<Vec<_>>();

    assert_eq!(calls.len(), 1, "expected one forced function call");
    assert_eq!(calls[0].name, "get_weather");
    assert!(
        calls[0].arguments.contains("Zurich"),
        "expected Zurich in function arguments: {}",
        calls[0].arguments
    );
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY or compatible endpoint credentials (costs real money)"]
async fn openai_completions_live_invalid_model_classifies_provider_error() {
    let client = live_client();
    let request = CreateCompletionRequest::user_text(
        "definitely-not-a-real-openai-model-for-lightspeed-tests",
        "hello",
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
