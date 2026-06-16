use llm_clients::ProviderFailureKind;
use llm_clients::openai::responses::{
    API_KIND, Client, CompactResponseRequest, Config, CountInputTokensRequest,
    CreateResponseRequest, FunctionTool, InputMessage, InputMessageContent, ListInputItemsRequest,
    MessageRole, ResponseInput, ResponseInputItem, ResponseStatus, RetrieveResponseRequest, Tool,
    ToolChoice,
};
use serde_json::json;
use std::collections::BTreeMap;

mod support;

use support::{
    env_or_dotenv_var, openai_responses_cancel, openai_responses_compact,
    openai_responses_count_input_tokens, openai_responses_create, openai_responses_delete,
    openai_responses_list_input_items, openai_responses_retrieve, openai_responses_stream,
    required_env_or_dotenv_var,
};

fn live_model() -> String {
    env_or_dotenv_var("OPENAI_RESPONSES_MODEL")
        .or_else(|_| env_or_dotenv_var("OPENAI_LIVE_MODEL"))
        .unwrap_or_else(|_| "gpt-5.5".to_string())
}

fn live_client() -> Client {
    let api_key = required_env_or_dotenv_var(
        "OPENAI_API_KEY",
        "OPENAI_API_KEY must be set in env or root .env to run openai:responses live tests",
    );

    let mut config = Config::new(api_key);
    if let Ok(base_url) = env_or_dotenv_var("OPENAI_BASE_URL") {
        config.base_url = base_url;
    }
    if let Ok(org_id) = env_or_dotenv_var("OPENAI_ORG_ID") {
        config.organization = Some(org_id);
    }
    if let Ok(project) = env_or_dotenv_var("OPENAI_PROJECT_ID") {
        config.project = Some(project);
    }

    Client::new(config).expect("OpenAI Responses client")
}

fn message_item(role: MessageRole, content: impl Into<String>) -> ResponseInputItem {
    ResponseInputItem::Message(InputMessage {
        role,
        content: InputMessageContent::Text(content.into()),
        extra: BTreeMap::new(),
    })
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_create_text() {
    let client = live_client();
    let request = CreateResponseRequest::text(
        live_model(),
        "Reply with exactly these two words: lightspeed transport",
    );

    let response = openai_responses_create(&client, request)
        .await
        .expect("create response");

    assert_eq!(response.status, 200);
    assert!(!response.parsed.id.is_empty());
    assert_eq!(response.parsed.status, Some(ResponseStatus::Completed));
    assert!(
        response
            .parsed
            .output_text()
            .to_lowercase()
            .contains("lightspeed"),
        "expected visible text output, got {:?}",
        response.parsed.output
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
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_stream_text() {
    let client = live_client();
    let request = CreateResponseRequest::text(live_model(), "Reply with exactly: streaming ok");
    let mut stream = openai_responses_stream(&client, request)
        .await
        .expect("stream response");

    let mut saw_delta = false;
    let mut saw_terminal = false;
    while let Some(event) = stream.next_event().await.expect("stream event") {
        if event.parsed.r#type == "response.output_text.delta" {
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

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_retrieve_response() {
    let client = live_client();
    let mut request = CreateResponseRequest::text(live_model(), "Reply with exactly: retrieve ok");
    request.store = Some(true);
    let created = openai_responses_create(&client, request)
        .await
        .expect("create response");

    let retrieved = openai_responses_retrieve(
        &client,
        &created.parsed.id,
        RetrieveResponseRequest::default(),
    )
    .await
    .expect("retrieve response");

    assert_eq!(retrieved.status, 200);
    assert_eq!(retrieved.parsed.id, created.parsed.id);
    assert_eq!(retrieved.parsed.status, Some(ResponseStatus::Completed));
    assert!(
        retrieved
            .parsed
            .output_text()
            .to_lowercase()
            .contains("retrieve"),
        "expected retrieved output text, got {:?}",
        retrieved.parsed.output
    );
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_list_input_items() {
    let client = live_client();
    let mut request = CreateResponseRequest::text(live_model(), "Reply with exactly: inputs ok");
    request.store = Some(true);
    let created = openai_responses_create(&client, request)
        .await
        .expect("create response");

    let items = openai_responses_list_input_items(
        &client,
        &created.parsed.id,
        ListInputItemsRequest::default(),
    )
    .await
    .expect("list input items");

    assert_eq!(items.status, 200);
    assert!(
        !items.parsed.data.is_empty(),
        "expected at least one response input item"
    );
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_manual_history_input_items() {
    let client = live_client();
    let phrase = "lightspeed-manual-history-7319";
    let request = CreateResponseRequest {
        model: Some(live_model()),
        input: Some(ResponseInput::Items(vec![
            message_item(
                MessageRole::User,
                format!("Remember this exact phrase: {phrase}. Reply with exactly: stored"),
            ),
            message_item(MessageRole::Assistant, "stored"),
            message_item(
                MessageRole::User,
                "What exact phrase did I ask you to remember? Reply with only the phrase.",
            ),
        ])),
        ..CreateResponseRequest::default()
    };

    let response = openai_responses_create(&client, request)
        .await
        .expect("create response with manual history");

    assert_eq!(response.status, 200);
    assert_eq!(response.parsed.status, Some(ResponseStatus::Completed));
    assert!(
        response.parsed.output_text().contains(phrase),
        "expected manual-history response to contain {phrase}, got {:?}",
        response.parsed.output
    );
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_previous_response_id_history() {
    let client = live_client();
    let phrase = "lightspeed-linked-history-8642";
    let mut first = CreateResponseRequest::text(
        live_model(),
        format!(
            "Remember this exact phrase for the next turn: {phrase}. Reply with exactly: stored"
        ),
    );
    first.store = Some(true);

    let created = openai_responses_create(&client, first)
        .await
        .expect("create stored response for continuation");
    assert_eq!(created.status, 200);
    assert_eq!(created.parsed.status, Some(ResponseStatus::Completed));
    assert!(!created.parsed.id.is_empty());

    let mut follow_up = CreateResponseRequest::text(
        live_model(),
        "What exact phrase did I ask you to remember? Reply with only the phrase.",
    );
    follow_up.previous_response_id = Some(created.parsed.id);

    let response = openai_responses_create(&client, follow_up)
        .await
        .expect("create response with previous_response_id");

    assert_eq!(response.status, 200);
    assert_eq!(response.parsed.status, Some(ResponseStatus::Completed));
    assert!(
        response.parsed.output_text().contains(phrase),
        "expected previous_response_id response to contain {phrase}, got {:?}",
        response.parsed.output
    );
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_count_input_tokens() {
    let client = live_client();
    let request = CountInputTokensRequest::text(live_model(), "Count these input tokens.");

    let tokens = openai_responses_count_input_tokens(&client, request)
        .await
        .expect("count input tokens");

    assert_eq!(tokens.status, 200);
    assert!(tokens.parsed.input_tokens > 0, "expected input tokens");
    assert_eq!(
        tokens.parsed.object.as_deref(),
        Some("response.input_tokens")
    );
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_delete_response() {
    let client = live_client();
    let mut request = CreateResponseRequest::text(live_model(), "Reply with exactly: delete ok");
    request.store = Some(true);
    let created = openai_responses_create(&client, request)
        .await
        .expect("create response");

    let deleted = openai_responses_delete(&client, &created.parsed.id)
        .await
        .expect("delete response");

    assert_eq!(deleted.status, 200);
    assert_eq!(deleted.parsed.id, created.parsed.id);
    assert!(deleted.parsed.deleted);
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_cancel_background_response() {
    let client = live_client();
    let mut request = CreateResponseRequest::text(
        live_model(),
        "Write a long numbered list of implementation notes. Keep going until stopped.",
    );
    request.store = Some(true);
    request.max_output_tokens = Some(10_000);
    request.extra.insert("background".to_string(), json!(true));
    let created = openai_responses_create(&client, request)
        .await
        .expect("create background response");

    let cancelled = openai_responses_cancel(&client, &created.parsed.id)
        .await
        .expect("cancel background response");

    assert_eq!(cancelled.status, 200);
    assert_eq!(cancelled.parsed.id, created.parsed.id);
    assert_eq!(cancelled.parsed.status, Some(ResponseStatus::Cancelled));
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_compact_response() {
    let client = live_client();
    let request = CompactResponseRequest::text(
        live_model(),
        "Summarize this short context for future continuation: Lightspeed is rewriting llm-clients as provider-native API wrappers.",
    );

    let compacted = openai_responses_compact(&client, request)
        .await
        .expect("compact response");

    assert_eq!(compacted.status, 200);
    assert!(!compacted.parsed.id.is_empty());
    assert!(
        !compacted.parsed.output.is_empty(),
        "expected compaction output"
    );
    assert!(
        compacted
            .parsed
            .usage
            .as_ref()
            .and_then(|usage| usage.total_tokens)
            .unwrap_or_default()
            > 0,
        "expected compaction usage"
    );
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_forced_function_call() {
    let client = live_client();
    let mut tool = FunctionTool::new(
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
    tool.description = Some("Get the current weather for a city".to_string());
    tool.strict = Some(true);

    let mut request = CreateResponseRequest::text(
        live_model(),
        "Call get_weather for Zurich. Do not answer in natural language.",
    );
    request.tools = Some(vec![Tool::Function(tool)]);
    request.tool_choice = Some(ToolChoice::Function {
        r#type: llm_clients::openai::responses::FunctionToolType::Function,
        name: "get_weather".to_string(),
    });

    let response = openai_responses_create(&client, request)
        .await
        .expect("function call response");
    let calls = response.parsed.function_calls().collect::<Vec<_>>();

    assert_eq!(calls.len(), 1, "expected one forced function call");
    assert_eq!(calls[0].name, "get_weather");
    assert!(
        calls[0].arguments.contains("Zurich"),
        "expected Zurich in function arguments: {}",
        calls[0].arguments
    );
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_responses_live_invalid_model_classifies_provider_error() {
    let client = live_client();
    let request = CreateResponseRequest::text(
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
