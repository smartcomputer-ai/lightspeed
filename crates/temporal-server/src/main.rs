use std::{env, net::SocketAddr, sync::Arc, time::Duration};

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use temporal_server::{
    config::pg_store_from_env,
    gateway::{
        DEFAULT_GATEWAY_BIND, DEFAULT_MAX_REQUEST_BODY_BYTES, DEFAULT_TASK_QUEUE,
        DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, GatewayAgentApi, GatewayServerConfig,
        gateway_router, serve_gateway,
    },
    worker::{self, WorkerActivities, WorkerServerConfig},
};

#[derive(Debug, Parser)]
#[command(
    name = "server",
    about = "Run the Forge hosted runtime",
    after_help = "When no command is supplied, server runs `both`."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Run only the HTTP/JSON-RPC gateway")]
    Gateway(GatewayArgs),
    #[command(about = "Run only the Temporal worker")]
    Worker(WorkerArgs),
    #[command(about = "Run the gateway and Temporal worker in one process")]
    Both(BothArgs),
}

#[derive(Clone, Debug, Args)]
struct TemporalArgs {
    #[arg(long, env = "FORGE_TASK_QUEUE", default_value = DEFAULT_TASK_QUEUE)]
    task_queue: String,

    #[arg(long, env = "TEMPORAL_ADDRESS", default_value = DEFAULT_TEMPORAL_TARGET)]
    temporal_target: String,

    #[arg(long, env = "TEMPORAL_NAMESPACE", default_value = DEFAULT_TEMPORAL_NAMESPACE)]
    namespace: String,
}

#[derive(Clone, Debug, Args)]
struct GatewayArgs {
    #[arg(long, env = "FORGE_GATEWAY_BIND", default_value = DEFAULT_GATEWAY_BIND)]
    bind: SocketAddr,

    #[command(flatten)]
    temporal: TemporalArgs,

    #[arg(
        long,
        env = "FORGE_GATEWAY_MAX_REQUEST_BODY_BYTES",
        default_value_t = DEFAULT_MAX_REQUEST_BODY_BYTES
    )]
    max_request_body_bytes: usize,
}

#[derive(Clone, Debug, Args)]
struct WorkerArgs {
    #[command(flatten)]
    temporal: TemporalArgs,
}

#[derive(Clone, Debug, Args)]
struct BothArgs {
    #[arg(long, env = "FORGE_GATEWAY_BIND", default_value = DEFAULT_GATEWAY_BIND)]
    bind: SocketAddr,

    #[command(flatten)]
    temporal: TemporalArgs,

    #[arg(
        long,
        env = "FORGE_GATEWAY_MAX_REQUEST_BODY_BYTES",
        default_value_t = DEFAULT_MAX_REQUEST_BODY_BYTES
    )]
    max_request_body_bytes: usize,
}

impl TemporalArgs {
    fn from_env() -> Self {
        Self {
            task_queue: env::var("FORGE_TASK_QUEUE")
                .unwrap_or_else(|_| DEFAULT_TASK_QUEUE.to_owned()),
            temporal_target: env::var("TEMPORAL_ADDRESS")
                .unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned()),
            namespace: env::var("TEMPORAL_NAMESPACE")
                .unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned()),
        }
    }
}

impl BothArgs {
    fn from_env() -> anyhow::Result<Self> {
        let bind = env::var("FORGE_GATEWAY_BIND")
            .unwrap_or_else(|_| DEFAULT_GATEWAY_BIND.to_owned())
            .parse()
            .with_context(|| "invalid FORGE_GATEWAY_BIND")?;
        let max_request_body_bytes = env::var("FORGE_GATEWAY_MAX_REQUEST_BODY_BYTES")
            .ok()
            .map(|value| {
                value
                    .parse()
                    .with_context(|| "invalid FORGE_GATEWAY_MAX_REQUEST_BODY_BYTES")
            })
            .transpose()?
            .unwrap_or(DEFAULT_MAX_REQUEST_BODY_BYTES);
        Ok(Self {
            bind,
            temporal: TemporalArgs::from_env(),
            max_request_body_bytes,
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Gateway(args)) => {
            serve_gateway(GatewayServerConfig {
                bind: args.bind,
                task_queue: args.temporal.task_queue,
                temporal_target: args.temporal.temporal_target,
                namespace: args.temporal.namespace,
                max_request_body_bytes: args.max_request_body_bytes,
            })
            .await
        }
        Some(Command::Worker(args)) => {
            worker::run_worker(WorkerServerConfig {
                task_queue: args.temporal.task_queue,
                temporal_target: args.temporal.temporal_target,
                namespace: args.temporal.namespace,
            })
            .await
        }
        Some(Command::Both(args)) => run_both(args).await,
        None => run_both(BothArgs::from_env()?).await,
    }
}

async fn run_both(args: BothArgs) -> anyhow::Result<()> {
    let runtime = worker::core_runtime()?;
    let client = temporal_server::gateway::connect_temporal(
        &args.temporal.temporal_target,
        &args.temporal.namespace,
    )
    .await?;
    let store = pg_store_from_env().await?;
    let activities = WorkerActivities::from_pg_store_with_default_runtime(store.clone())?;
    let mut temporal_worker = worker::worker_with_activities(
        &runtime,
        client.clone(),
        args.temporal.task_queue.clone(),
        activities,
    )?;
    let shutdown_worker = temporal_worker.shutdown_handle();
    let worker_future = temporal_worker.run();
    tokio::pin!(worker_future);

    let api = Arc::new(
        GatewayAgentApi::builder(client, store)
            .with_task_queue(args.temporal.task_queue)
            .build(),
    );
    let app = gateway_router(api, args.max_request_body_bytes);
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    eprintln!(
        "server listening on http://{} with embedded worker",
        args.bind
    );
    let gateway_future = async {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
    };
    tokio::pin!(gateway_future);

    tokio::select! {
        worker_result = worker_future.as_mut() => {
            match worker_result {
                Ok(()) => anyhow::bail!("Temporal worker stopped while gateway was still running"),
                Err(error) => Err(error.context("Temporal worker failed")),
            }
        }
        gateway_result = gateway_future.as_mut() => {
            shutdown_worker();
            tokio::time::timeout(Duration::from_secs(10), worker_future.as_mut())
                .await
                .map_err(|_| anyhow::anyhow!("Temporal worker did not shut down within 10 seconds"))??;
            gateway_result?;
            Ok(())
        }
    }
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("failed to listen for shutdown signal: {error}");
    }
}
