mod identity_cli;

use std::{env, net::SocketAddr, sync::Arc, time::Duration};

use clap::{Args, Parser, Subcommand};
use temporal_server::{
    config::{
        DeploymentStores, TaskQueues, cas_sweep_grace_from_env, environment_public_url_from_env,
        gateway_auth_mode_from_env, postgres_pool_from_env, task_queues_from_env,
    },
    gateway::{
        DEFAULT_GATEWAY_BIND, DEFAULT_MAX_REQUEST_BODY_BYTES, DEFAULT_TEMPORAL_NAMESPACE,
        DEFAULT_TEMPORAL_TARGET, GatewayRoutes, GatewayState, gateway_router,
        prewarm_single_universe,
    },
    roles::{Role, RoleSet, TaskTypes},
    universe::UniverseRuntime,
    worker::{self, BotWorkerActivities, ChannelWorkerActivities, WorkerActivities},
};
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Debug, Parser)]
#[command(
    name = "lightspeed-server",
    version = release_info::LONG_VERSION,
    about = "Run the Lightspeed hosted runtime",
    after_help = "When no command is supplied, the server runs every role in this process: \
gateway, sessions, bots, channels. Select a subset with --roles (or LIGHTSPEED_ROLES) and \
split worker roles into workflow-only or activity-only pollers with --task-types."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    run: RunArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Apply embedded PostgreSQL migrations and update the schema ledger")]
    Migrate,
    #[command(
        name = "schema-version",
        about = "Print the current and required PostgreSQL schema revisions"
    )]
    SchemaVersion,
    #[command(
        name = "cas-sweep",
        about = "Run one blob-collection pass over every universe and print its statistics"
    )]
    CasSweep {
        /// Report what a pass would delete without deleting anything.
        #[arg(long)]
        dry_run: bool,
    },
    #[command(subcommand, about = "Manage universes (tenants) of this deployment")]
    Universe(UniverseCommand),
    #[command(
        subcommand,
        name = "api-key",
        about = "Manage inbound gateway API keys"
    )]
    ApiKey(ApiKeyCommand),
    #[command(
        subcommand,
        about = "Host administration of canonical identity and access records"
    )]
    Identity(identity_cli::IdentityCommand),
}

#[derive(Debug, Subcommand)]
enum UniverseCommand {
    #[command(about = "Create a universe (generates an id when omitted)")]
    Create {
        #[arg(long)]
        universe_id: Option<uuid::Uuid>,
        /// Active deployment administrator that becomes this universe's Admin.
        #[arg(long)]
        creator_principal: uuid::Uuid,
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
        #[arg(
            long,
            required_unless_present = "deployment",
            conflicts_with = "deployment"
        )]
        universe_id: Option<uuid::Uuid>,
        #[arg(long)]
        deployment: bool,
        #[arg(long)]
        name: Option<String>,
        /// Canonical principal authenticated by this key.
        #[arg(long)]
        principal: uuid::Uuid,
        /// Active issuing actor (trusted host administration).
        #[arg(long)]
        actor_principal: uuid::Uuid,
    },
    #[command(about = "List API keys (prefixes only; secrets are never stored)")]
    List,
    #[command(about = "Revoke an API key by its display prefix")]
    Revoke {
        key_prefix: String,
        #[arg(long)]
        actor_principal: uuid::Uuid,
    },
}

#[derive(Clone, Debug, Args)]
struct RunArgs {
    /// Roles this process runs: a comma-separated subset of gateway,
    /// environment-gateway, sessions, bots, channels (default: all). Run
    /// exactly one environment-gateway process per deployment.
    #[arg(long, env = "LIGHTSPEED_ROLES")]
    roles: Option<String>,

    /// Task types the worker roles poll: all, workflows, or activities.
    #[arg(long, env = "LIGHTSPEED_WORKER_TASK_TYPES")]
    task_types: Option<String>,

    #[arg(long, env = "LIGHTSPEED_GATEWAY_BIND", default_value = DEFAULT_GATEWAY_BIND)]
    bind: SocketAddr,

    /// Sessions task queue. Deployments sharing a Temporal namespace must
    /// set distinct queues.
    #[arg(long, env = "LIGHTSPEED_TASK_QUEUE")]
    task_queue: Option<String>,

    #[arg(long, env = "LIGHTSPEED_TASK_QUEUE_BOTS")]
    bots_task_queue: Option<String>,

    #[arg(long, env = "LIGHTSPEED_TASK_QUEUE_CHANNELS")]
    channels_task_queue: Option<String>,

    #[arg(long, env = "TEMPORAL_ADDRESS", default_value = DEFAULT_TEMPORAL_TARGET)]
    temporal_target: String,

    #[arg(long, env = "TEMPORAL_NAMESPACE", default_value = DEFAULT_TEMPORAL_NAMESPACE)]
    namespace: String,

    #[arg(
        long,
        env = "LIGHTSPEED_GATEWAY_MAX_REQUEST_BODY_BYTES",
        default_value_t = DEFAULT_MAX_REQUEST_BODY_BYTES
    )]
    max_request_body_bytes: usize,

    /// Externally reachable base URL of the gateway (OAuth callbacks,
    /// webhook ingest URLs). Defaults to http://{bind}.
    #[arg(long, env = "LIGHTSPEED_PUBLIC_BASE_URL")]
    public_base_url: Option<String>,
}

impl RunArgs {
    fn roles(&self) -> anyhow::Result<RoleSet> {
        RoleSet::parse(self.roles.as_deref().unwrap_or("")).map_err(|error| anyhow::anyhow!(error))
    }

    fn task_types(&self) -> anyhow::Result<TaskTypes> {
        TaskTypes::parse(self.task_types.as_deref().unwrap_or(""))
            .map_err(|error| anyhow::anyhow!(error))
    }

    fn task_queues(&self) -> anyhow::Result<TaskQueues> {
        let mut queues = task_queues_from_env()?;
        if let Some(queue) = self.task_queue.as_deref().filter(|value| !value.is_empty()) {
            queues.sessions = queue.to_owned();
        }
        if let Some(queue) = self
            .bots_task_queue
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            queues.bots = queue.to_owned();
        }
        if let Some(queue) = self
            .channels_task_queue
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            queues.channels = queue.to_owned();
        }
        Ok(queues)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    init_logging()?;
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Migrate) => run_migrate().await,
        Some(Command::SchemaVersion) => run_schema_version().await,
        Some(Command::CasSweep { dry_run }) => run_cas_sweep(dry_run).await,
        Some(Command::Universe(command)) => run_universe_command(command).await,
        Some(Command::ApiKey(command)) => run_api_key_command(command).await,
        Some(Command::Identity(command)) => identity_cli::run(command).await,
        None => run_roles(cli.run).await,
    }
}

async fn run_migrate() -> anyhow::Result<()> {
    let pool = postgres_pool_from_env().await?;
    let before = store_pg::schema_status(&pool).await?;
    println!("current_schema_revision: {}", before.current_revision);
    println!("required_schema_revision: {}", before.required_revision);
    store_pg::PgStore::migrate(&pool).await?;
    let after = store_pg::verify_schema(&pool).await?;
    println!("applied_schema_revision: {}", after.current_revision);
    Ok(())
}

async fn run_schema_version() -> anyhow::Result<()> {
    let pool = postgres_pool_from_env().await?;
    let status = store_pg::schema_status(&pool).await?;
    println!("current_schema_revision: {}", status.current_revision);
    println!("required_schema_revision: {}", status.required_revision);
    if status.is_current() {
        Ok(())
    } else {
        anyhow::bail!(
            "database migration required: current revision {}, required revision {}",
            status.current_revision,
            status.required_revision
        )
    }
}

async fn run_cas_sweep(dry_run: bool) -> anyhow::Result<()> {
    let Some(grace) = cas_sweep_grace_from_env()? else {
        anyhow::bail!(
            "blob collection is disabled: LIGHTSPEED_CAS_SWEEP_GRACE_MS is 0; set a positive grace to sweep"
        );
    };
    let stores = DeploymentStores::from_env().await?;
    let sweeper = worker::CasBlobSweeper::new(stores, grace);
    let stats = sweeper.run_once(dry_run).await?;
    println!("dry_run: {dry_run}");
    println!("grace_ms: {}", grace.as_millis());
    println!("universes_scanned: {}", stats.universes_scanned);
    println!("rows_scanned: {}", stats.rows_scanned);
    println!("leader_busy: {}", stats.leader_busy);
    println!("candidates: {}", stats.candidates);
    println!("rows_deleted: {}", stats.rows_deleted);
    println!("bytes_freed: {}", stats.bytes_freed);
    println!("objects_deleted: {}", stats.objects_deleted);
    println!("object_errors: {}", stats.object_errors);
    println!("holder_conflicts: {}", stats.holder_conflicts);
    println!("errors: {}", stats.errors);
    Ok(())
}

async fn run_universe_command(command: UniverseCommand) -> anyhow::Result<()> {
    let stores = DeploymentStores::from_env().await?;
    match command {
        UniverseCommand::Create {
            universe_id,
            creator_principal,
            slug,
        } => {
            let universe_id = universe_id.unwrap_or_else(uuid::Uuid::new_v4);
            use access::AccessStore as _;
            store_pg::PgAccessStore::new(stores.pool().clone())
                .apply(
                    creator_principal,
                    access::AccessChange::CreateUniverse {
                        universe_id,
                        slug: slug.clone(),
                    },
                    identity_cli::now_ms()?,
                )
                .await?;
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
            actor_principal,
            deployment: _,
        } => {
            let scope = universe_id.map_or(access::AccessScope::Deployment, |universe_id| {
                access::AccessScope::Universe { universe_id }
            });
            let minted = auth::mint_api_key(scope, principal, actor_principal, name, now_ms);
            api_keys
                .create_api_key(auth::CreateApiKey {
                    authority_scope: access::AccessScope::Deployment,
                    key_hash: minted.key_hash,
                    record: minted.record.clone(),
                })
                .await?;
            println!("key_prefix: {}", minted.record.key_prefix);
            println!("scope: {}", serde_json::to_string(&scope)?);
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
                    serde_json::to_string(&record.scope)?,
                    status,
                    record.display_name.as_deref().unwrap_or("-"),
                );
            }
            Ok(())
        }
        ApiKeyCommand::Revoke {
            key_prefix,
            actor_principal,
        } => {
            let record = api_keys
                .list_api_keys()
                .await?
                .into_iter()
                .find(|key| key.key_prefix == key_prefix)
                .ok_or_else(|| anyhow::anyhow!("unknown api key prefix"))?;
            if api_keys
                .revoke_managed_key(
                    actor_principal,
                    access::AccessScope::Deployment,
                    record.scope,
                    &key_prefix,
                    now_ms,
                )
                .await?
                .is_some()
            {
                println!("revoked: {key_prefix}");
                Ok(())
            } else {
                anyhow::bail!("no api key with prefix {key_prefix}")
            }
        }
    }
}

/// Compose the selected roles in one process over one universe registry,
/// one Temporal client, and one blob cache. Every worker role is its own
/// Temporal worker on its own task queue; the gateway role adds the HTTP
/// server and the deployment reconcilers.
async fn run_roles(args: RunArgs) -> anyhow::Result<()> {
    let roles = args.roles()?;
    let task_types = args.task_types()?;
    let task_queues = args.task_queues()?;
    let mode = gateway_auth_mode_from_env()?;
    let runtime = worker::core_runtime()?;
    let client =
        temporal_server::gateway::connect_temporal(&args.temporal_target, &args.namespace).await?;
    let stores = DeploymentStores::from_env()
        .await?
        .with_blob_cache(temporal_server::config::blob_cache_from_env()?);
    let reaper_stores = stores.clone();
    let public_base_url = args
        .public_base_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", args.bind));
    let universes = Arc::new(
        UniverseRuntime::new_with_environment_gateway(
            client.clone(),
            task_queues.sessions.clone(),
            Some(public_base_url.clone()),
            stores,
            roles.has(Role::EnvironmentGateway),
        )?
        .with_task_queues(task_queues.clone()),
    );
    prewarm_single_universe(&mode, &universes).await?;

    tracing::info!(
        target: "temporal_server",
        roles = %roles,
        task_types = %task_types,
        temporal_target = %args.temporal_target,
        namespace = %args.namespace,
        sessions_queue = %task_queues.sessions,
        bots_queue = %task_queues.bots,
        channels_queue = %task_queues.channels,
        "lightspeed-server starting"
    );

    let mut background: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut workers: Vec<(Role, temporalio_sdk::Worker)> = Vec::new();

    if roles.has(Role::EnvironmentGateway) {
        background.push(tokio::spawn(universes.clone().run_environment_reconciler()));
        background.push(tokio::spawn(universes.clone().run_power_reaper()));
    }
    if roles.has(Role::Sessions) {
        let activities = WorkerActivities::with_runtime(universes.clone());
        workers.push((
            Role::Sessions,
            worker::sessions_worker(
                &runtime,
                client.clone(),
                task_queues.sessions.clone(),
                activities,
                task_types.worker_task_types(),
            )?,
        ));
        background.push(tokio::spawn(
            worker::PromiseReaper::new(client.clone(), reaper_stores.clone()).run_forever(),
        ));
        background.push(tokio::spawn(
            worker::SessionRetentionReaper::new(reaper_stores.clone()).run_forever(),
        ));
        match cas_sweep_grace_from_env()? {
            Some(grace) => background.push(tokio::spawn(
                worker::CasBlobSweeper::new(reaper_stores, grace).run_forever(),
            )),
            None => tracing::info!(
                target: "temporal_server",
                "cas blob sweeper disabled by LIGHTSPEED_CAS_SWEEP_GRACE_MS=0"
            ),
        }
    }
    if roles.has(Role::Bots) {
        let activities = BotWorkerActivities::with_runtime(universes.clone());
        workers.push((
            Role::Bots,
            worker::bots_worker(
                &runtime,
                client.clone(),
                task_queues.bots.clone(),
                activities,
                task_types.worker_task_types(),
            )?,
        ));
        background.push(tokio::spawn(
            universes.clone().run_bot_schedule_reconciler(),
        ));
    }
    if roles.has(Role::Channels) {
        let activities = ChannelWorkerActivities::with_runtime(universes.clone());
        workers.push((
            Role::Channels,
            worker::channels_worker(
                &runtime,
                client.clone(),
                task_queues.channels.clone(),
                activities,
                task_types.worker_task_types(),
            )?,
        ));
    }

    let mut shutdowns = Vec::new();
    let mut worker_futures = Vec::new();
    for (role, mut temporal_worker) in workers {
        shutdowns.push(temporal_worker.shutdown_handle());
        worker_futures.push(Box::pin(async move {
            let result = temporal_worker.run().await;
            (role, result)
        }));
    }

    let gateway_future: std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>>>> =
        if roles.serves_http() {
            let routes = GatewayRoutes {
                api: roles.has(Role::Gateway),
                environment: roles.has(Role::EnvironmentGateway),
            };
            let gateway_state = Arc::new(
                GatewayState::multi(mode, universes, public_base_url)
                    .with_environment_public_url(environment_public_url_from_env()?),
            );
            let app = gateway_router(gateway_state, args.max_request_body_bytes, routes);
            let listener = tokio::net::TcpListener::bind(args.bind).await?;
            tracing::info!(
                target: "temporal_server",
                bind = %args.bind,
                api_routes = routes.api,
                environment_routes = routes.environment,
                "gateway listening"
            );
            Box::pin(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown_signal())
                    .await?;
                Ok(())
            })
        } else {
            Box::pin(async {
                shutdown_signal().await;
                Ok(())
            })
        };
    tokio::pin!(gateway_future);

    let stop_background = |background: &Vec<tokio::task::JoinHandle<()>>| {
        for task in background {
            task.abort();
        }
    };

    if worker_futures.is_empty() {
        let result = gateway_future.await;
        stop_background(&background);
        return result;
    }

    let workers_future = futures::future::select_all(worker_futures);
    tokio::pin!(workers_future);
    tokio::select! {
        (worker_result, _index, remaining) = workers_future.as_mut() => {
            stop_background(&background);
            for shutdown in shutdowns {
                shutdown();
            }
            let _ = tokio::time::timeout(Duration::from_secs(10), futures::future::join_all(remaining)).await;
            match worker_result {
                (role, Ok(())) => anyhow::bail!("{role} worker stopped while the process was still running"),
                (role, Err(error)) => Err(error.context(format!("{role} worker failed"))),
            }
        }
        gateway_result = gateway_future.as_mut() => {
            stop_background(&background);
            for shutdown in shutdowns {
                shutdown();
            }
            tokio::time::timeout(Duration::from_secs(10), async {
                let (_first, _index, remaining) = workers_future.as_mut().await;
                futures::future::join_all(remaining).await;
            })
            .await
            .map_err(|_| anyhow::anyhow!("Temporal workers did not shut down within 10 seconds"))?;
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
