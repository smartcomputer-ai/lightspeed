use std::{net::SocketAddr, sync::Arc};

use api::{JsonRpcRequest, JsonRpcResponse, dispatch_json_rpc};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
};
use serde::Deserialize;
use store_pg::PgStore;
use temporalio_client::Client;

use crate::config::pg_store_from_env;

use super::{GatewayAgentApi, OAuthCallbackOutcome, connect_temporal};

pub const DEFAULT_GATEWAY_BIND: &str = "127.0.0.1:18080";
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct GatewayServerConfig {
    pub bind: SocketAddr,
    pub task_queue: String,
    pub temporal_target: String,
    pub namespace: String,
    pub max_request_body_bytes: usize,
    /// Externally reachable base URL for the OAuth callback
    /// (`{base}/auth/callback`). Defaults to `http://{bind}`.
    pub public_base_url: Option<String>,
}

pub async fn serve_gateway(config: GatewayServerConfig) -> anyhow::Result<()> {
    let client = connect_temporal(&config.temporal_target, &config.namespace).await?;
    let store = pg_store_from_env().await?;
    serve_gateway_with_client_store(client, store, config).await
}

pub async fn serve_gateway_with_client_store(
    client: Client,
    store: Arc<PgStore>,
    config: GatewayServerConfig,
) -> anyhow::Result<()> {
    let public_base_url = public_base_url_or_default(&config);
    let api = Arc::new(
        GatewayAgentApi::builder(client, store)
            .with_task_queue(config.task_queue)
            .with_public_base_url(public_base_url)
            .build(),
    );
    let app = gateway_router(api, config.max_request_body_bytes);
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(target: "temporal_server", bind = %config.bind, "gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}

pub fn public_base_url_or_default(config: &GatewayServerConfig) -> String {
    config
        .public_base_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", config.bind))
}

pub fn gateway_router(api: Arc<GatewayAgentApi>, max_request_body_bytes: usize) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/rpc", post(rpc))
        .route("/auth/callback", get(oauth_callback))
        .route("/auth/client-metadata.json", get(cimd_document))
        .layer(DefaultBodyLimit::max(max_request_body_bytes))
        .with_state(api)
}

/// Client ID Metadata Document (draft-ietf-oauth-client-id-metadata-document):
/// authorization servers fetch this to resolve Lightspeed's CIMD client id.
async fn cimd_document(State(api): State<Arc<GatewayAgentApi>>) -> Json<serde_json::Value> {
    Json(api.cimd_document())
}

async fn rpc(
    State(api): State<Arc<GatewayAgentApi>>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    Json(dispatch_json_rpc(api.as_ref(), request).await)
}

/// Query parameters of the OAuth authorization callback (RFC 6749 §4.1.2).
/// `code` is a one-time secret credential; this handler must never log it.
#[derive(Deserialize)]
struct OAuthCallbackQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

async fn oauth_callback(
    State(api): State<Arc<GatewayAgentApi>>,
    Query(query): Query<OAuthCallbackQuery>,
) -> (StatusCode, Html<String>) {
    let callback = auth::AuthCallback {
        state: query.state.unwrap_or_default(),
        code: query.code.map(auth::SecretValue::new),
        error: query.error,
        error_description: query.error_description,
    };
    match api.complete_oauth_callback(callback).await {
        OAuthCallbackOutcome::Completed { grant_id } => (
            StatusCode::OK,
            callback_page(
                "Authorization complete",
                &format!(
                    "Lightspeed stored the credential as grant {}. You can close this window.",
                    html_escape(&grant_id)
                ),
            ),
        ),
        OAuthCallbackOutcome::Failed { message } => (
            StatusCode::OK,
            callback_page(
                "Authorization failed",
                &format!(
                    "The authorization did not complete: {}. You can close this window and retry with a new login.",
                    html_escape(&message)
                ),
            ),
        ),
        OAuthCallbackOutcome::Rejected { message } => (
            StatusCode::BAD_REQUEST,
            callback_page(
                "Invalid authorization callback",
                &format!(
                    "{}. Start a new login and try again.",
                    html_escape(&message)
                ),
            ),
        ),
    }
}

fn callback_page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font-family: system-ui, sans-serif; max-width: 40rem; margin: 4rem auto;\">\
         <h1>{title}</h1><p>{body}</p></body></html>"
    ))
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
