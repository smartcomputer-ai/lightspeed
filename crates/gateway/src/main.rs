use std::{net::SocketAddr, sync::Arc};

use api::{JsonRpcRequest, JsonRpcResponse, dispatch_json_rpc};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    routing::{get, post},
};
use clap::Parser;
use gateway::{
    DEFAULT_TASK_QUEUE, DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, GatewayAgentApi,
    connect_temporal, pg_store_from_env,
};

const DEFAULT_GATEWAY_BIND: &str = "127.0.0.1:18080";
const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(name = "gateway", about = "Run the Forge Agent JSON-RPC gateway")]
struct Args {
    #[arg(long, env = "FORGE_GATEWAY_BIND", default_value = DEFAULT_GATEWAY_BIND)]
    bind: SocketAddr,

    #[arg(long, env = "FORGE_TASK_QUEUE", default_value = DEFAULT_TASK_QUEUE)]
    task_queue: String,

    #[arg(long, env = "TEMPORAL_ADDRESS", default_value = DEFAULT_TEMPORAL_TARGET)]
    temporal_target: String,

    #[arg(long, env = "TEMPORAL_NAMESPACE", default_value = DEFAULT_TEMPORAL_NAMESPACE)]
    namespace: String,

    #[arg(
        long,
        env = "FORGE_GATEWAY_MAX_REQUEST_BODY_BYTES",
        default_value_t = DEFAULT_MAX_REQUEST_BODY_BYTES
    )]
    max_request_body_bytes: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let args = Args::parse();
    let client = connect_temporal(&args.temporal_target, &args.namespace).await?;
    let store = pg_store_from_env().await?;
    let api = Arc::new(
        GatewayAgentApi::builder(client, store)
            .with_task_queue(args.task_queue)
            .build(),
    );

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/rpc", post(rpc))
        .layer(DefaultBodyLimit::max(args.max_request_body_bytes))
        .with_state(api);
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    eprintln!("gateway listening on http://{}", args.bind);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn rpc(
    State(api): State<Arc<GatewayAgentApi>>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    Json(dispatch_json_rpc(api.as_ref(), request).await)
}
