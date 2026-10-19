use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use llm_clients::openai::completions::CreateCompletionRequest;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scenario {
    delay_ms: u64,
    #[serde(default)]
    jitter_ms: u64,
    seed: u64,
    response_bytes: usize,
    prompt_tokens: u64,
    completion_tokens: u64,
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    rounds: Vec<Vec<Call>>,
    #[serde(default)]
    failure: Option<Failure>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    name: String,
    arguments: Value,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    status: u16,
    code: String,
    retry_after_seconds: u32,
}
#[derive(Default)]
struct Metrics {
    requests: AtomicU64,
    inflight: AtomicU64,
    peak: AtomicU64,
    errors: AtomicU64,
    elapsed_us: AtomicU64,
    response_bytes: AtomicU64,
}
struct App {
    scenarios: BTreeMap<String, Scenario>,
    metrics: Metrics,
    started: Instant,
}
fn app(scenarios: BTreeMap<String, Scenario>) -> Result<Router, String> {
    if scenarios.is_empty() {
        return Err("at least one scenario is required".into());
    }
    for s in scenarios.values() {
        if s.response_bytes > 8 * 1024 * 1024
            || s.delay_ms > 300_000
            || s.jitter_ms > 300_000
            || s.cached_tokens > s.prompt_tokens
            || s.prompt_tokens.checked_add(s.completion_tokens).is_none()
            || s.rounds.iter().any(|r| {
                r.is_empty()
                    || r.iter()
                        .any(|c| c.name.is_empty() || !c.arguments.is_object())
            })
            || s.failure
                .as_ref()
                .is_some_and(|f| ![400, 429, 503].contains(&f.status))
        {
            return Err("invalid scenario bounds, usage, tool calls or failure status".into());
        }
    }
    Ok(Router::new()
        .route("/v1/chat/completions", post(complete))
        .route("/metrics", get(metrics))
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(Arc::new(App {
            scenarios,
            metrics: Metrics::default(),
            started: Instant::now(),
        })))
}
async fn metrics(State(app): State<Arc<App>>) -> Json<Value> {
    let m = &app.metrics;
    Json(
        json!({"uptime_seconds": app.started.elapsed().as_secs_f64(),
        "requests": m.requests.load(Ordering::Relaxed), "inflight": m.inflight.load(Ordering::Relaxed),
        "peak_inflight": m.peak.load(Ordering::Relaxed), "errors": m.errors.load(Ordering::Relaxed),
        "handler_microseconds_total": m.elapsed_us.load(Ordering::Relaxed),
        "response_bytes": m.response_bytes.load(Ordering::Relaxed)}),
    )
}
fn error(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"error":{"message":code,"type":"simulator_error","code":code}})),
    )
        .into_response()
}
async fn complete(
    State(app): State<Arc<App>>,
    body: Result<Json<CreateCompletionRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let start = Instant::now();
    let m = &app.metrics;
    m.requests.fetch_add(1, Ordering::Relaxed);
    let active = m.inflight.fetch_add(1, Ordering::Relaxed) + 1;
    m.peak.fetch_max(active, Ordering::Relaxed);
    let response = match body {
        Ok(Json(request)) => respond(&app, request).await,
        Err(_) => error(StatusCode::BAD_REQUEST, "invalid_request"),
    };
    m.inflight.fetch_sub(1, Ordering::Relaxed);
    m.elapsed_us
        .fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
    if !response.status().is_success() {
        m.errors.fetch_add(1, Ordering::Relaxed);
    }
    response
}
async fn respond(app: &App, request: CreateCompletionRequest) -> Response {
    let Some(s) = app.scenarios.get(&request.model) else {
        return error(StatusCode::BAD_REQUEST, "unknown_scenario");
    };
    if request.stream == Some(true) || request.extra.get("n").is_some_and(|n| n != &json!(1)) {
        return error(StatusCode::BAD_REQUEST, "unsupported_operation");
    }
    let Some(last_user) = request.messages.iter().rposition(|m| m.role == "user") else {
        return error(StatusCode::BAD_REQUEST, "missing_user_message");
    };
    // Conversation-derived progress: sessions, retries and prior turns never share a counter.
    let tail = &request.messages[last_user + 1..];
    let mut round = 0;
    let mut pending = Vec::new();
    for message in tail {
        if message.role == "assistant" {
            if !pending.is_empty() {
                return error(StatusCode::BAD_REQUEST, "missing_tool_results");
            }
            if let Some(calls) = &message.tool_calls {
                for call in calls {
                    let Some(id) = &call.id else {
                        return error(StatusCode::BAD_REQUEST, "missing_tool_id");
                    };
                    pending.push(id.clone());
                }
                round += 1;
            }
        } else if message.role == "tool" {
            let Some(index) = pending
                .iter()
                .position(|id| Some(id) == message.tool_call_id.as_ref())
            else {
                return error(StatusCode::BAD_REQUEST, "unexpected_tool_result");
            };
            pending.remove(index);
        }
    }
    if !pending.is_empty() {
        return error(StatusCode::BAD_REQUEST, "missing_tool_results");
    }
    let encoded = serde_json::to_vec(&request).expect("native request serializes");
    let hash = encoded.iter().fold(s.seed ^ 0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    });
    tokio::time::sleep(Duration::from_millis(s.delay_ms + hash % (s.jitter_ms + 1))).await;
    if let Some(f) = &s.failure {
        let mut response = error(
            StatusCode::from_u16(f.status).expect("validated status"),
            &f.code,
        );
        response.headers_mut().insert(
            "retry-after",
            f.retry_after_seconds
                .to_string()
                .parse()
                .expect("integer header"),
        );
        return response;
    }
    let (message, finish) = if let Some(calls) = s.rounds.get(round) {
        if calls.iter().any(|c| {
            !request
                .tools
                .as_ref()
                .is_some_and(|tools| tools.iter().any(|t| t.function.name == c.name))
        }) {
            return error(StatusCode::BAD_REQUEST, "scripted_tool_not_admitted");
        }
        (
            json!({"role":"assistant","content":null,"tool_calls":calls.iter().enumerate().map(|(i,c)| json!({
            "id":format!("call_{hash:016x}_{round}_{i}"),"type":"function",
            "function":{"name":c.name,"arguments":c.arguments.to_string()}
        })).collect::<Vec<_>>()}),
            "tool_calls",
        )
    } else {
        (
            json!({"role":"assistant","content":"x".repeat(s.response_bytes)}),
            "stop",
        )
    };
    let body = json!({"id":format!("chatcmpl_{hash:016x}"),"object":"chat.completion","created":1,"model":request.model,
        "choices":[{"index":0,"message":message,"finish_reason":finish}],
        "usage":{"prompt_tokens":s.prompt_tokens,"completion_tokens":s.completion_tokens,
            "total_tokens":s.prompt_tokens+s.completion_tokens,"prompt_tokens_details":{"cached_tokens":s.cached_tokens}}});
    let bytes = serde_json::to_vec(&body).expect("response serializes");
    app.metrics
        .response_bytes
        .fetch_add(bytes.len() as u64, Ordering::Relaxed);
    ([("content-type", "application/json")], bytes).into_response()
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: llm-simulator SCENARIOS.json [127.0.0.1:19090]")?;
    let address = args.next().unwrap_or_else(|| "127.0.0.1:19090".into());
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    let scenarios = serde_json::from_slice(&std::fs::read(path)?)?;
    let router = app(scenarios)?;
    let listener = tokio::net::TcpListener::bind(&address).await?;
    eprintln!("llm-simulator listening on {}", listener.local_addr()?);
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
