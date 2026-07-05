use std::{env, net::SocketAddr, sync::Arc, time::Duration};

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use temporal_server::{
    config::{DeploymentStores, gateway_auth_mode_from_env, task_queue_from_env},
    gateway::{
        DEFAULT_GATEWAY_BIND, DEFAULT_MAX_REQUEST_BODY_BYTES, DEFAULT_TEMPORAL_NAMESPACE,
        DEFAULT_TEMPORAL_TARGET, GatewayServerConfig, GatewayState, gateway_router,
        prewarm_single_universe, serve_gateway,
    },
    universe::UniverseRuntime,
    worker::{self, WorkerActivities, WorkerServerConfig},
};
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Debug, Parser)]
#[command(
    name = "server",
    about = "Run the Lightspeed hosted runtime",
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
    #[command(subcommand, about = "Manage universes (tenants) of this deployment")]
    Universe(UniverseCommand),
    #[command(
        subcommand,
        name = "api-key",
        about = "Manage inbound gateway API keys"
    )]
    ApiKey(ApiKeyCommand),
}

#[derive(Debug, Subcommand)]
enum UniverseCommand {
    #[command(about = "Create a universe (generates an id when omitted)")]
    Create {
        #[arg(long)]
        universe_id: Option<uuid::Uuid>,
        #[arg(long)]
        slug: Option<String>,
    },
    #[command(about = "List universes")]
    List,
}

#[derive(Debug, Subcommand)]
enum ApiKeyCommand {
    #[command(about = "Mint an API key for a universe; the secret prints exactly once")]
    Create {
        #[arg(long)]
        universe_id: uuid::Uuid,
        /// Display name shown in listings.
        #[arg(long)]
        name: Option<String>,
        /// Principal stamped onto grants created through this key:
        /// `user:<id>` or `service_account:<id>`. Defaults to the universe
        /// default principal.
        #[arg(long)]
        principal: Option<String>,
    },
    #[command(about = "List API keys (prefixes only; secrets are never stored)")]
    List,
    #[command(about = "Revoke an API key by its display prefix")]
    Revoke { key_prefix: String },
}

#[derive(Clone, Debug, Args)]
struct TemporalArgs {
    /// Temporal task queue shared by all universes of this deployment.
    /// Defaults to lightspeed-agent. Deployments sharing a Temporal namespace
    /// must set distinct queues.
    #[arg(long, env = "LIGHTSPEED_TASK_QUEUE")]
    task_queue: Option<String>,

    #[arg(long, env = "TEMPORAL_ADDRESS", default_value = DEFAULT_TEMPORAL_TARGET)]
    temporal_target: String,

    #[arg(long, env = "TEMPORAL_NAMESPACE", default_value = DEFAULT_TEMPORAL_NAMESPACE)]
    namespace: String,
}

#[derive(Clone, Debug, Args)]
struct GatewayArgs {
    #[arg(long, env = "LIGHTSPEED_GATEWAY_BIND", default_value = DEFAULT_GATEWAY_BIND)]
    bind: SocketAddr,

    #[command(flatten)]
    temporal: TemporalArgs,

    #[arg(
        long,
        env = "LIGHTSPEED_GATEWAY_MAX_REQUEST_BODY_BYTES",
        default_value_t = DEFAULT_MAX_REQUEST_BODY_BYTES
    )]
    max_request_body_bytes: usize,

    /// Externally reachable base URL for the OAuth callback. Defaults to
    /// http://{bind}.
    #[arg(long, env = "LIGHTSPEED_PUBLIC_BASE_URL")]
    public_base_url: Option<String>,
}

#[derive(Clone, Debug, Args)]
struct WorkerArgs {
    #[command(flatten)]
    temporal: TemporalArgs,
}

#[derive(Clone, Debug, Args)]
struct BothArgs {
    #[arg(long, env = "LIGHTSPEED_GATEWAY_BIND", default_value = DEFAULT_GATEWAY_BIND)]
    bind: SocketAddr,

    #[command(flatten)]
    temporal: TemporalArgs,

    #[arg(
        long,
        env = "LIGHTSPEED_GATEWAY_MAX_REQUEST_BODY_BYTES",
        default_value_t = DEFAULT_MAX_REQUEST_BODY_BYTES
    )]
    max_request_body_bytes: usize,

    /// Externally reachable base URL for the OAuth callback. Defaults to
    /// http://{bind}.
    #[arg(long, env = "LIGHTSPEED_PUBLIC_BASE_URL")]
    public_base_url: Option<String>,
}

impl TemporalArgs {
    fn from_env() -> Self {
        Self {
            task_queue: env::var("LIGHTSPEED_TASK_QUEUE").ok(),
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
        let bind = env::var("LIGHTSPEED_GATEWAY_BIND")
            .unwrap_or_else(|_| DEFAULT_GATEWAY_BIND.to_owned())
            .parse()
            .with_context(|| "invalid LIGHTSPEED_GATEWAY_BIND")?;
        let max_request_body_bytes = env::var("LIGHTSPEED_GATEWAY_MAX_REQUEST_BODY_BYTES")
            .ok()
            .map(|value| {
                value
                    .parse()
                    .with_context(|| "invalid LIGHTSPEED_GATEWAY_MAX_REQUEST_BODY_BYTES")
            })
            .transpose()?
            .unwrap_or(DEFAULT_MAX_REQUEST_BODY_BYTES);
        Ok(Self {
            bind,
            temporal: TemporalArgs::from_env(),
            max_request_body_bytes,
            public_base_url: env::var("LIGHTSPEED_PUBLIC_BASE_URL")
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
        Some(Command::Universe(command)) => run_universe_command(command).await,
        Some(Command::ApiKey(command)) => run_api_key_command(command).await,
        None => run_both(BothArgs::from_env()?).await,
    }
}

async fn run_universe_command(command: UniverseCommand) -> anyhow::Result<()> {
    let stores = DeploymentStores::from_env().await?;
    match command {
        UniverseCommand::Create { universe_id, slug } => {
            let universe_id = universe_id.unwrap_or_else(uuid::Uuid::new_v4);
            let store = stores.store_for_with_slug(universe_id, slug.clone());
            store.ensure_universe().await?;
            println!("universe_id: {universe_id}");
            if let Some(slug) = slug {
                println!("slug: {slug}");
            }
            Ok(())
        }
        UniverseCommand::List => {
            for (universe_id, slug) in store_pg::list_universes(stores.pool()).await? {
                match slug {
                    Some(slug) => println!("{universe_id}  {slug}"),
                    None => println!("{universe_id}"),
                }
            }
            Ok(())
        }
    }
}

async fn run_api_key_command(command: ApiKeyCommand) -> anyhow::Result<()> {
    use auth::ApiKeyStore as _;

    let stores = DeploymentStores::from_env().await?;
    let api_keys = store_pg::PgApiKeyStore::new(stores.pool().clone());
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    match command {
        ApiKeyCommand::Create {
            universe_id,
            name,
            principal,
        } => {
            if !store_pg::universe_exists(stores.pool(), universe_id).await? {
                anyhow::bail!(
                    "unknown universe: {universe_id} (create it first: server universe create)"
                );
            }
            let principal = parse_principal_arg(principal.as_deref())?;
            let minted = auth::mint_api_key(universe_id, principal, name, now_ms);
            api_keys
                .create_api_key(auth::CreateApiKey {
                    key_hash: minted.key_hash,
                    record: minted.record.clone(),
                })
                .await?;
            println!("key_prefix: {}", minted.record.key_prefix);
            println!("universe_id: {universe_id}");
            // The one and only time the secret leaves the process.
            println!("secret: {}", minted.secret.expose());
            Ok(())
        }
        ApiKeyCommand::List => {
            for record in api_keys.list_api_keys().await? {
                let status = if record.revoked_at_ms.is_some() {
                    "revoked"
                } else {
                    "active"
                };
                println!(
                    "{}  {}  {}  {}",
                    record.key_prefix,
                    record.universe_id,
                    status,
                    record.display_name.as_deref().unwrap_or("-"),
                );
            }
            Ok(())
        }
        ApiKeyCommand::Revoke { key_prefix } => {
            if api_keys.revoke_api_key(&key_prefix, now_ms).await? {
                println!("revoked: {key_prefix}");
                Ok(())
            } else {
                anyhow::bail!("no api key with prefix {key_prefix}")
            }
        }
    }
}

/// Parse `--principal user:<id>` / `service_account:<id>`; `None` is the
/// universe-default principal.
fn parse_principal_arg(value: Option<&str>) -> anyhow::Result<auth::PrincipalRef> {
    let Some(value) = value else {
        return Ok(auth::PrincipalRef::universe_default());
    };
    let (kind, id) = value
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("--principal must be user:<id> or service_account:<id>"))?;
    let kind = match kind {
        "user" => auth::PrincipalKind::User,
        "service_account" => auth::PrincipalKind::ServiceAccount,
        other => {
            anyhow::bail!("invalid principal kind {other:?}; expected user or service_account")
        }
    };
    if id.is_empty() {
        anyhow::bail!("--principal id must not be empty");
    }
    Ok(auth::PrincipalRef {
        kind,
        id: Some(id.to_owned()),
    })
}

async fn run_both(args: BothArgs) -> anyhow::Result<()> {
    let task_queue = args.temporal.resolved_task_queue()?;
    let mode = gateway_auth_mode_from_env()?;
    let runtime = worker::core_runtime()?;
    let client = temporal_server::gateway::connect_temporal(
        &args.temporal.temporal_target,
        &args.temporal.namespace,
    )
    .await?;
    // `both` mode: the gateway and worker share one process, one universe
    // registry, and therefore one blob cache.
    let stores = DeploymentStores::from_env()
        .await?
        .with_blob_cache(temporal_server::config::blob_cache_from_env()?);
    let public_base_url = args
        .public_base_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", args.bind));
    // Gateway and worker share one universe registry: fleet spawns and
    // activity routing hit the same lazily-built per-universe state.
    let universes = Arc::new(UniverseRuntime::new(
        client.clone(),
        task_queue.clone(),
        Some(public_base_url.clone()),
        stores,
    )?);
    prewarm_single_universe(&mode, &universes).await?;
    let activities = WorkerActivities::with_runtime(universes.clone());
    let mut temporal_worker =
        worker::worker_with_activities(&runtime, client.clone(), task_queue.clone(), activities)?;
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

    let gateway_state = Arc::new(GatewayState::multi(mode, universes, public_base_url));
    let app = gateway_router(gateway_state, args.max_request_body_bytes);
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
    match env::var("LIGHTSPEED_LOG_FORMAT")
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
            "invalid LIGHTSPEED_LOG_FORMAT={other:?}; expected one of: compact, pretty, json"
        ),
    }
    Ok(())
}
