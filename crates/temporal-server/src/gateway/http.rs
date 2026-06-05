use std::{net::SocketAddr, sync::Arc};

use api::{JsonRpcRequest, JsonRpcResponse, dispatch_json_rpc};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    routing::{get, post},
};
use store_pg::PgStore;
use temporalio_client::Client;

use crate::config::pg_store_from_env;

use super::{GatewayAgentApi, connect_temporal};

pub const DEFAULT_GATEWAY_BIND: &str = "127.0.0.1:18080";
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct GatewayServerConfig {
    pub bind: SocketAddr,
    pub task_queue: String,
    pub temporal_target: String,
    pub namespace: String,
    pub max_request_body_bytes: usize,
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
    let api = Arc::new(
        GatewayAgentApi::builder(client, store)
            .with_task_queue(config.task_queue)
            .build(),
    );
    let app = gateway_router(api, config.max_request_body_bytes);
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(target: "temporal_server", bind = %config.bind, "gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}

pub fn gateway_router(api: Arc<GatewayAgentApi>, max_request_body_bytes: usize) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/rpc", post(rpc))
        .layer(DefaultBodyLimit::max(max_request_body_bytes))
        .with_state(api)
}

async fn rpc(
    State(api): State<Arc<GatewayAgentApi>>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    Json(dispatch_json_rpc(api.as_ref(), request).await)
}
