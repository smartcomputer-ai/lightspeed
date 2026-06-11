use std::{env, net::SocketAddr, sync::Arc, time::Duration};

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use temporal_server::{
    config::{pg_store_from_env, task_queue_from_env},
    gateway::{
        DEFAULT_GATEWAY_BIND, DEFAULT_MAX_REQUEST_BODY_BYTES, DEFAULT_TEMPORAL_NAMESPACE,
        DEFAULT_TEMPORAL_TARGET, GatewayAgentApi, GatewayServerConfig, gateway_router,
        serve_gateway,
    },
    worker::{self, WorkerActivities, WorkerServerConfig},
};
use tracing_subscriber::{EnvFilter, fmt};

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
    /// Temporal task queue. Defaults to forge-universe-{FORGE_PG_UNIVERSE_ID}.
    #[arg(long, env = "FORGE_TASK_QUEUE")]
    task_queue: Option<String>,

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

    /// Externally reachable base URL for the OAuth callback. Defaults to
    /// http://{bind}.
    #[arg(long, env = "FORGE_PUBLIC_BASE_URL")]
    public_base_url: Option<String>,
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

    /// Externally reachable base URL for the OAuth callback. Defaults to
    /// http://{bind}.
    #[arg(long, env = "FORGE_PUBLIC_BASE_URL")]
    public_base_url: Option<String>,
}

impl TemporalArgs {
    fn from_env() -> Self {
        Self {
            task_queue: env::var("FORGE_TASK_QUEUE").ok(),
            temporal_target: env::var("TEMPORAL_ADDRESS")
                .unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned()),
            namespace: env::var("TEMPORAL_NAMESPACE")
                .unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned()),
        }
    }

    fn resolved_task_queue(&self) -> anyhow::Result<String> {
        match self.task_queue.as_deref().filter(|value| !value.is_empty()) {
            Some(task_queue) => Ok(task_queue.to_owned()),
            None => task_queue_from_env(),
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
            public_base_url: env::var("FORGE_PUBLIC_BASE_URL")
                .ok()
                .filter(|value| !value.is_empty()),
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    init_logging()?;
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Gateway(args)) => {
            serve_gateway(GatewayServerConfig {
                bind: args.bind,
                task_queue: args.temporal.resolved_task_queue()?,
                temporal_target: args.temporal.temporal_target,
                namespace: args.temporal.namespace,
                max_request_body_bytes: args.max_request_body_bytes,
                public_base_url: args.public_base_url,
            })
            .await
        }
        Some(Command::Worker(args)) => {
            worker::run_worker(WorkerServerConfig {
                task_queue: args.temporal.resolved_task_queue()?,
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
    let task_queue = args.temporal.resolved_task_queue()?;
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
        task_queue.clone(),
        activities,
    )?;
    let shutdown_worker = temporal_worker.shutdown_handle();
    let worker_future = temporal_worker.run();
    tokio::pin!(worker_future);

    tracing::info!(
        target: "temporal_server",
        temporal_target = %args.temporal.temporal_target,
        namespace = %args.temporal.namespace,
        task_queue = %task_queue,
        "temporal worker polling"
    );

    let api = Arc::new(
        GatewayAgentApi::builder(client, store)
            .with_task_queue(task_queue)
            .with_public_base_url(
                args.public_base_url
                    .clone()
                    .unwrap_or_else(|| format!("http://{}", args.bind)),
            )
            .build(),
    );
    let app = gateway_router(api, args.max_request_body_bytes);
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    tracing::info!(target: "temporal_server", bind = %args.bind, "gateway listening");
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
        tracing::warn!(target: "temporal_server", %error, "failed to listen for shutdown signal");
    }
}

fn init_logging() -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("warn,temporal_server=info,temporal_workflow=info,temporalio_sdk_core=info")
    });
    match env::var("FORGE_LOG_FORMAT")
        .unwrap_or_else(|_| "compact".to_owned())
        .as_str()
    {
        "json" => fmt()
            .with_env_filter(env_filter)
            .json()
            .try_init()
            .map_err(|error| anyhow::anyhow!("{error}"))?,
        "pretty" => fmt()
            .with_env_filter(env_filter)
            .pretty()
            .try_init()
            .map_err(|error| anyhow::anyhow!("{error}"))?,
        "compact" | "" => fmt()
            .with_env_filter(env_filter)
            .compact()
            .try_init()
            .map_err(|error| anyhow::anyhow!("{error}"))?,
        other => anyhow::bail!(
            "invalid FORGE_LOG_FORMAT={other:?}; expected one of: compact, pretty, json"
        ),
    }
    Ok(())
}
