use super::*;
use llm_clients::{
    EndpointOverride, RequestAuth,
    openai::completions::{Client, CompletionMessage, CompletionTool, Config},
};

async fn server() -> (String, tokio::task::JoinHandle<()>) {
    let mut scenarios: BTreeMap<String, Scenario> = serde_json::from_str(include_str!(
        "../../../scripts/perf/fixtures/simulator.json"
    ))
    .unwrap();
    let mut tool = scenarios["perf-text"].clone();
    tool.rounds = vec![vec![Call {
        name: "read".into(),
        arguments: json!({"path":"/fixture"}),
    }]];
    scenarios.insert("tools".into(), tool);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = app(scenarios).unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (url, task)
}
#[tokio::test(flavor = "current_thread")]
async fn native_client_endpoint_usage_and_concurrent_conversations() {
    let (url, server) = server().await;
    let endpoint = EndpointOverride::from_parts(&format!("{url}/v1"), &BTreeMap::new()).unwrap();
    let client = Client::new(Config::without_api_key()).unwrap();
    let run = |text: &'static str| {
        let client = client.clone();
        let endpoint = endpoint.clone();
        async move {
            let mut request = CreateCompletionRequest::user_text("tools", text);
            request.tools = Some(vec![CompletionTool::function(
                "read",
                json!({"type":"object"}),
            )]);
            let first = client
                .create_with_transport(request.clone(), Some(RequestAuth::None), Some(&endpoint))
                .await
                .unwrap()
                .parsed;
            assert_eq!(
                first.choices[0].finish_reason.as_deref(),
                Some("tool_calls")
            );
            let repeated = client
                .create_with_transport(request.clone(), Some(RequestAuth::None), Some(&endpoint))
                .await
                .unwrap()
                .parsed;
            assert_eq!(first, repeated);
            let message = first.choices[0].message.clone().unwrap();
            let id = message.tool_calls.as_ref().unwrap()[0].id.clone();
            request.messages.push(message);
            request.messages.push(CompletionMessage {
                role: "tool".into(),
                tool_call_id: id,
                content: Some(
                    llm_clients::openai::completions::CompletionMessageContent::Text("ok".into()),
                ),
                ..Default::default()
            });
            let final_answer = client
                .create_with_transport(request.clone(), Some(RequestAuth::None), Some(&endpoint))
                .await
                .unwrap()
                .parsed;
            assert_eq!(final_answer.output_text().len(), 256);
            assert_eq!(final_answer.usage.unwrap().total_tokens, Some(164));
            request.messages.push(CompletionMessage::user("next turn"));
            let next = client
                .create_with_transport(request, Some(RequestAuth::None), Some(&endpoint))
                .await
                .unwrap()
                .parsed;
            assert_eq!(next.tool_calls().count(), 1);
            first.id
        }
    };
    let (a, b) = tokio::join!(run("session a"), run("session b"));
    assert_ne!(a, b);
    server.abort();
}
#[tokio::test(flavor = "current_thread")]
async fn failures_and_unsupported_requests_are_explicit_and_counted() {
    let (url, server) = server().await;
    let http = reqwest::Client::new();
    for (model, status) in [
        ("perf-rate-limit", 429),
        ("perf-unavailable", 503),
        ("perf-context-limit", 400),
        ("missing", 400),
        ("tools", 400),
    ] {
        let response = http
            .post(format!("{url}/v1/chat/completions"))
            .json(&CreateCompletionRequest::user_text(model, "test"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
    }
    let response = http
        .post(format!("{url}/v1/chat/completions"))
        .json(&json!({"model":"perf-text","messages":[],"stream":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let metrics: Value = http
        .get(format!("{url}/metrics"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metrics["errors"], 6);
    assert_eq!(metrics["inflight"], 0);
    server.abort();
}

#[test]
fn invalid_scenarios_fail_before_listening() {
    let scenarios: BTreeMap<String, Scenario> = serde_json::from_str(include_str!(
        "../../../scripts/perf/fixtures/simulator.json"
    ))
    .unwrap();
    for invalid in [
        Scenario {
            cached_tokens: 101,
            ..scenarios["perf-text"].clone()
        },
        Scenario {
            response_bytes: 9 * 1024 * 1024,
            ..scenarios["perf-text"].clone()
        },
        Scenario {
            rounds: vec![vec![]],
            ..scenarios["perf-text"].clone()
        },
    ] {
        assert!(app(BTreeMap::from([("invalid".into(), invalid)])).is_err());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn retry_headers_and_incomplete_tool_round_are_rejected() {
    let (url, server) = server().await;
    let http = reqwest::Client::new();
    let response = http
        .post(format!("{url}/v1/chat/completions"))
        .json(&CreateCompletionRequest::user_text(
            "perf-rate-limit",
            "retry",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["retry-after"], "1");
    let response = http.post(format!("{url}/v1/chat/completions"))
        .json(&json!({"model":"tools", "messages":[
            {"role":"user","content":"test"},
            {"role":"assistant","tool_calls":[{"id":"a","type":"function","function":{"name":"read","arguments":"{}"}}]}
        ]})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "missing_tool_results");
    server.abort();
}
