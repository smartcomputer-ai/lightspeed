use std::{env, net::SocketAddr, sync::Arc, time::Duration};

use clap::{Args, Parser, Subcommand};
use temporal_runtime::{
    config::{
        DeploymentStores, TaskQueues, cas_sweep_grace_from_env, gateway_auth_mode_from_env,
        postgres_pool_from_env, task_queues_from_env,
    },
    gateway::{
        DEFAULT_GATEWAY_BIND, DEFAULT_MAX_REQUEST_BODY_BYTES, DEFAULT_TEMPORAL_NAMESPACE,
        DEFAULT_TEMPORAL_TARGET, GatewayRoutes, GatewayState, TemporalConnectionSettings,
        gateway_router, prewarm_single_universe, registration, replica::GatewayReplica,
    },
    roles::{Role, RoleSet},
    universe::UniverseRuntime,
    worker::{
        self, BotWorkerActivities, ChannelWorkerActivities, CodeWorkerActivities, WorkerActivities,
    },
};
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Debug, Parser)]
#[command(
    name = "lightspeed-runtime",
    version = release_info::LONG_VERSION,
    about = "Run the Lightspeed hosted runtime",
    after_help = "When no command is supplied, the server runs every role in this process: \
gateway, environment-gateway, sessions, bots, channels, code. Select a subset with --roles \
(or LIGHTSPEED_ROLES). Each worker role runs its workflows and activities together."
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
}

#[derive(Debug, Subcommand)]
enum UniverseCommand {
    #[command(about = "Create a universe (generates an id when omitted)")]
    Create {
        #[arg(long)]
        universe_id: Option<uuid::Uuid>,
        /// The new universe's handle; derived from the id when omitted. An
        /// existing universe keeps the handle it has.
        #[arg(long)]
        handle: Option<String>,
    },
    #[command(about = "List universes")]
    List,
}

#[derive(Clone, Debug, Args)]
struct RunArgs {
    /// Roles this process runs: a comma-separated subset of gateway,
    /// environment-gateway, sessions, bots, channels, code (default: all).
    /// Environment gateway replicas advertise their own direct peer URLs.
    #[arg(long, env = "LIGHTSPEED_ROLES")]
    roles: Option<String>,

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

    #[arg(long, env = "LIGHTSPEED_TASK_QUEUE_CODE")]
    code_task_queue: Option<String>,

    /// Maximum simultaneous JavaScript interpreters in this process.
    #[arg(long, env = "LIGHTSPEED_CODE_MAX_CONCURRENT_EXECUTIONS", default_value_t = temporal_runtime::config::DEFAULT_CODE_MAX_CONCURRENT_EXECUTIONS)]
    code_max_concurrent_executions: usize,

    #[arg(long, env = "TEMPORAL_ADDRESS", default_value = DEFAULT_TEMPORAL_TARGET)]
    temporal_target: String,

    #[arg(long, env = "TEMPORAL_NAMESPACE", default_value = DEFAULT_TEMPORAL_NAMESPACE)]
    namespace: String,

    /// Connect to Temporal over TLS: true or false. Unset, TLS is on when
    /// another TLS setting or an API key is given, or the address is an
    /// https:// URL.
    #[arg(long, env = "TEMPORAL_TLS", value_name = "BOOL")]
    temporal_tls: Option<String>,

    /// PEM file of the CA that issued the Temporal server certificate.
    /// Without it the operating system's trusted roots are used.
    #[arg(long, env = "TEMPORAL_TLS_SERVER_CA_FILE", value_name = "PATH")]
    temporal_tls_server_ca_file: Option<String>,

    /// Name to verify the Temporal server certificate against instead of
    /// the host in the address.
    #[arg(long, env = "TEMPORAL_TLS_SERVER_NAME", value_name = "NAME")]
    temporal_tls_server_name: Option<String>,

    /// PEM file of the client certificate presented to Temporal. Needs
    /// the key file too.
    #[arg(long, env = "TEMPORAL_TLS_CLIENT_CERT_FILE", value_name = "PATH")]
    temporal_tls_client_cert_file: Option<String>,

    /// PEM file of the client certificate's private key.
    #[arg(long, env = "TEMPORAL_TLS_CLIENT_KEY_FILE", value_name = "PATH")]
    temporal_tls_client_key_file: Option<String>,

    /// File holding the Temporal API key. The key itself can be given in
    /// TEMPORAL_API_KEY instead; it has no flag, so that it never shows in
    /// a process listing.
    #[arg(long, env = "TEMPORAL_API_KEY_FILE", value_name = "PATH")]
    temporal_api_key_file: Option<String>,

    #[arg(
        long,
        env = "LIGHTSPEED_GATEWAY_MAX_REQUEST_BODY_BYTES",
        default_value_t = DEFAULT_MAX_REQUEST_BODY_BYTES
    )]
    max_request_body_bytes: usize,

    /// Externally reachable base URL of the gateway (OAuth callbacks,
    /// webhook ingest URLs, and the origin registered daemons sign).
    /// Defaults to http://{bind}.
    #[arg(long, env = "LIGHTSPEED_PUBLIC_BASE_URL")]
    public_base_url: Option<String>,

    /// Further gateway URLs registered daemons may dial and sign, comma
    /// separated: for example a cluster-internal Service address. Only
    /// their origins count.
    #[arg(
        long,
        env = "LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS",
        value_delimiter = ','
    )]
    environment_registration_urls: Vec<String>,
}

impl RunArgs {
    fn roles(&self) -> anyhow::Result<RoleSet> {
        RoleSet::parse(self.roles.as_deref().unwrap_or("")).map_err(|error| anyhow::anyhow!(error))
    }

    /// The Temporal connection these arguments describe. Every flag already
    /// carries its environment variable; `api_key` is the one setting read
    /// from the environment alone.
    fn temporal_settings(&self, api_key: Option<String>) -> TemporalConnectionSettings {
        TemporalConnectionSettings {
            address: Some(self.temporal_target.clone()),
            namespace: Some(self.namespace.clone()),
            tls: self.temporal_tls.clone(),
            tls_server_ca_file: self.temporal_tls_server_ca_file.clone(),
            tls_server_name: self.temporal_tls_server_name.clone(),
            tls_client_cert_file: self.temporal_tls_client_cert_file.clone(),
            tls_client_key_file: self.temporal_tls_client_key_file.clone(),
            api_key,
            api_key_file: self.temporal_api_key_file.clone(),
        }
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
        if let Some(queue) = self
            .code_task_queue
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            queues.code = queue.to_owned();
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
        None => run_roles(cli.run).await,
    }
}

async fn run_migrate() -> anyhow::Result<()> {
    let pool = postgres_pool_from_env().await?;
    let before = store_pg::schema_status(&pool)
        .await
        .map_err(explain_migration_error)?;
    println!("current_schema_revision: {}", before.current_revision);
    println!("required_schema_revision: {}", before.required_revision);
    store_pg::PgStore::migrate(&pool)
        .await
        .map_err(explain_migration_error)?;
    let after = store_pg::verify_schema(&pool)
        .await
        .map_err(explain_migration_error)?;
    println!("applied_schema_revision: {}", after.current_revision);
    Ok(())
}

async fn run_schema_version() -> anyhow::Result<()> {
    let pool = postgres_pool_from_env().await?;
    let status = store_pg::schema_status(&pool)
        .await
        .map_err(explain_migration_error)?;
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

fn explain_migration_error(error: store_pg::PgStoreError) -> anyhow::Error {
    let mismatch = match error {
        store_pg::PgStoreError::MigrationChecksumChanged { version, name, .. } => {
            format!("migration {version} ({name}) has a different checksum")
        }
        store_pg::PgStoreError::MigrationNameChanged {
            version, expected, ..
        } => format!("migration {version} ({expected}) has a different name"),
        other => return other.into(),
    };
    anyhow::anyhow!(
        "database schema does not match this build: {mismatch}.\n\
         If this is disposable local development data, run `./dev.sh reset` and retry. Reset deletes local PostgreSQL and MinIO data.\n\
         To keep the data, restore the migration used by this database and put changes in a new migration; do not edit the migration ledger."
    )
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
    println!("walks_completed: {}", stats.walks_completed);
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
            handle,
        } => {
            let universe_id = universe_id.unwrap_or_else(uuid::Uuid::new_v4);
            if let Some(handle) = &handle
                && !store_pg::is_valid_universe_handle(handle)
            {
                anyhow::bail!(
                    "invalid handle {handle}: use 1-63 lowercase letters, digits and hyphens, starting and ending with a letter or digit, and not a UUID"
                );
            }
            stores
                .store_for_with_handle(universe_id, handle)
                .ensure_universe()
                .await?;
            println!("universe_id: {universe_id}");
            if let Some(universe) =
                store_pg::read_universe_stats(stores.pool(), universe_id).await?
            {
                println!("handle: {}", universe.handle);
            }
            Ok(())
        }
        UniverseCommand::List => {
            for (universe_id, handle) in store_pg::list_universes(stores.pool()).await? {
                println!("{universe_id}  {handle}");
            }
            Ok(())
        }
    }
}

/// Compose the selected roles in one process over one universe registry,
/// one Temporal client, and one blob cache. Every worker role is its own
/// Temporal worker on its own task queue; the gateway role adds the HTTP
/// server and the deployment reconcilers.
async fn run_roles(args: RunArgs) -> anyhow::Result<()> {
    temporal_runtime::config::validate_model_environment()?;
    let roles = args.roles()?;
    let task_queues = args.task_queues()?;
    let mode = gateway_auth_mode_from_env()?;
    let runtime = worker::worker_runtime()?;
    let temporal = args
        .temporal_settings(TemporalConnectionSettings::from_env()?.api_key)
        .resolve()?;
    let client = temporal_runtime::gateway::connect_temporal(&temporal).await?;
    let stores = DeploymentStores::from_env()
        .await?
        .with_blob_cache(temporal_runtime::config::blob_cache_from_env()?);
    let reaper_stores = stores.clone();
    // Daemons sign the gateway origin they dial; refuse to serve
    // registration under an origin the gateway cannot name.
    let registration_origins = if roles.has(Role::EnvironmentGateway) {
        temporal_runtime::gateway::http::registration_origins(
            args.public_base_url.as_deref(),
            &args.environment_registration_urls,
            args.bind,
        )?
    } else {
        Vec::new()
    };
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

    // Bind before publishing this instance's address. No connections are
    // accepted until its lease and the role workers have been initialized.
    let listener = if roles.serves_http() {
        Some(tokio::net::TcpListener::bind(args.bind).await?)
    } else {
        None
    };
    let gateway_state = if roles.serves_http() {
        let mut state = GatewayState::multi(mode, universes.clone(), public_base_url)
            .with_registration_origins(registration_origins);
        if roles.has(Role::EnvironmentGateway) {
            let peer_url = GatewayReplica::peer_url_from_env()?;
            let instance_roles: Vec<_> =
                roles.iter().map(|role| role.as_str().to_owned()).collect();
            let replica = GatewayReplica::register(
                universes.stores().pool(),
                &instance_roles,
                peer_url.as_deref(),
            )
            .await?;
            tracing::info!(instance = %replica.instance_id(), peer_url = ?replica.peer_url(), "environment gateway instance registered");
            state = state.with_replica(replica);
        }
        Some(Arc::new(state))
    } else {
        None
    };

    tracing::info!(
        target: "temporal_runtime",
        roles = %roles,
        temporal_target = %args.temporal_target,
        namespace = %temporal.namespace(),
        temporal_tls = temporal.tls().is_some(),
        temporal_client_certificate = temporal.uses_client_certificate(),
        temporal_api_key = temporal.uses_api_key(),
        sessions_queue = %task_queues.sessions,
        bots_queue = %task_queues.bots,
        channels_queue = %task_queues.channels,
        code_queue = %task_queues.code,
        "lightspeed-runtime starting"
    );

    let mut background: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut workers: Vec<(Role, temporalio_sdk::Worker)> = Vec::new();

    if roles.has(Role::EnvironmentGateway) {
        let state = gateway_state
            .as_ref()
            .expect("environment role has an HTTP listener");
        background.push(tokio::spawn(
            state.replica().clone().maintain(state.clone()),
        ));
        background.push(tokio::spawn(universes.clone().run_environment_reconciler()));
        background.push(tokio::spawn(registration::run_registered_sweep(
            universes.clone(),
        )));
    }
    background.push(tokio::spawn(universes.clone().run_universe_invalidations()));
    if roles.has(Role::Sessions) {
        background.push(tokio::spawn(universes.clone().run_universe_deletion()));
        let activities = WorkerActivities::with_runtime(universes.clone());
        workers.push((
            Role::Sessions,
            worker::sessions_worker(
                &runtime,
                client.clone(),
                task_queues.sessions.clone(),
                activities,
            )?,
        ));
        background.push(tokio::spawn(
            // Checkpoint reads for a whole universe would evict the workers' hot entries.
            worker::PromiseReaper::new(client.clone(), reaper_stores.clone().with_blob_cache(None))
                .run_forever(),
        ));
        background.push(tokio::spawn(
            worker::SessionRetentionReaper::new(reaper_stores.clone()).run_forever(),
        ));
        match cas_sweep_grace_from_env()? {
            Some(grace) => background.push(tokio::spawn(
                worker::CasBlobSweeper::new(reaper_stores, grace).run_forever(),
            )),
            None => tracing::info!(
                target: "temporal_runtime",
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
            )?,
        ));
    }

    if roles.has(Role::Code) {
        let activities = CodeWorkerActivities::with_runtime(
            universes.clone(),
            args.code_max_concurrent_executions,
        )?;
        workers.push((
            Role::Code,
            worker::code_worker(
                &runtime,
                client.clone(),
                task_queues.code.clone(),
                activities,
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
        if let (Some(state), Some(listener)) = (gateway_state.clone(), listener) {
            let routes = GatewayRoutes {
                api: roles.has(Role::Gateway),
                environment: roles.has(Role::EnvironmentGateway),
            };
            let app = gateway_router(state.clone(), args.max_request_body_bytes, routes);
            tracing::info!(
                target: "temporal_runtime",
                bind = %args.bind,
                api_routes = routes.api,
                environment_routes = routes.environment,
                "gateway listening"
            );
            Box::pin(async move {
                use std::future::IntoFuture as _;
                let (stop_http, http_stopped) = tokio::sync::oneshot::channel();
                let server = axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = http_stopped.await;
                    })
                    .into_future();
                tokio::pin!(server);
                let lease_lost = tokio::select! {
                    result = server.as_mut() => return result.map_err(anyhow::Error::from),
                    _ = shutdown_signal() => false,
                    _ = state.replica().stopped(), if routes.environment => true,
                };
                if routes.environment {
                    // Keep the listener running so readiness probes and
                    // already-issued daemon dial-backs can finish during drain.
                    tokio::select! {
                        result = server.as_mut() => return result.map_err(anyhow::Error::from),
                        _ = state.replica().drain(&state) => {},
                    }
                }
                let _ = stop_http.send(());
                tokio::time::timeout(Duration::from_secs(5), server.as_mut())
                    .await
                    .map_err(|_| anyhow::anyhow!("gateway HTTP shutdown exceeded 5 seconds"))??;
                anyhow::ensure!(!lease_lost, "environment gateway instance lease expired");
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

    let result = if worker_futures.is_empty() {
        gateway_future.await
    } else {
        let workers_future = futures::future::select_all(worker_futures);
        tokio::pin!(workers_future);
        tokio::select! {
        (worker_result, _index, remaining) = workers_future.as_mut() => {
            for shutdown in shutdowns {
                shutdown();
            }
            let _ = tokio::time::timeout(Duration::from_secs(10), futures::future::join_all(remaining)).await;
            match worker_result {
                (role, Ok(())) => Err(anyhow::anyhow!("{role} worker stopped while the process was still running")),
                (role, Err(error)) => Err(anyhow::Error::from(error).context(format!("{role} worker failed"))),
            }
        }
        gateway_result = gateway_future.as_mut() => {
            for shutdown in shutdowns {
                shutdown();
            }
            let workers_stopped = tokio::time::timeout(Duration::from_secs(10), async {
                let (_first, _index, remaining) = workers_future.as_mut().await;
                futures::future::join_all(remaining).await;
            })
            .await
            .map_err(|_| anyhow::anyhow!("Temporal workers did not shut down within 10 seconds"));
            gateway_result.and(workers_stopped)
        }
        }
    };
    if let Some(state) = gateway_state.filter(|_| roles.has(Role::EnvironmentGateway)) {
        state.replica().drain(&state).await;
        state.replica().unregister(universes.stores().pool()).await;
    }
    stop_background(&background);
    result
}

/// Resolves on SIGINT from a terminal or SIGTERM from a process manager such as
/// Kubernetes. A signal that cannot be watched leaves the other one in charge
/// rather than stopping a healthy process.
async fn shutdown_signal() {
    let interrupt = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(target: "temporal_runtime", %error, "failed to listen for SIGINT");
            std::future::pending::<()>().await;
        }
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                terminate.recv().await;
            }
            Err(error) => {
                tracing::warn!(target: "temporal_runtime", %error, "failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}

fn init_logging() -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("warn,temporal_runtime=info,temporal_workflow=info,temporalio_sdk_core=info")
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

#[cfg(test)]
mod tests {
    use super::*;
    use temporal_runtime::gateway::TemporalConfigError;

    #[tokio::test(flavor = "current_thread")]
    async fn sigterm_starts_graceful_shutdown() {
        let shutdown = shutdown_signal();
        tokio::pin!(shutdown);
        // The first poll installs the handlers, so the signal below cannot kill
        // this test process.
        tokio::time::timeout(Duration::from_millis(1), &mut shutdown)
            .await
            .expect_err("no signal was sent yet");
        let status = tokio::process::Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .await
            .expect("run kill");
        assert!(status.success());
        tokio::time::timeout(Duration::from_secs(10), shutdown)
            .await
            .expect("SIGTERM must resolve the shutdown signal");
    }

    #[test]
    fn worker_deployment_is_selected_by_role() {
        let cli = Cli::try_parse_from(["lightspeed-runtime", "--roles", "sessions,bots"])
            .expect("select worker roles");
        assert_eq!(cli.run.roles().unwrap().to_string(), "sessions,bots");
        let error = Cli::try_parse_from(["lightspeed-runtime", "--task-types", "workflows"])
            .expect_err("removed polling override must be rejected");
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn temporal_flags_describe_one_connection() {
        let cli = Cli::try_parse_from([
            "lightspeed-runtime",
            "--temporal-target",
            "temporal.internal:7233",
            "--namespace",
            "lightspeed",
            "--temporal-tls",
            "true",
            "--temporal-tls-server-ca-file",
            "/etc/temporal/ca.pem",
            "--temporal-tls-server-name",
            "frontend.temporal.internal",
            "--temporal-tls-client-cert-file",
            "/etc/temporal/client.pem",
            "--temporal-tls-client-key-file",
            "/etc/temporal/client.key",
            "--temporal-api-key-file",
            "/run/secrets/temporal-api-key",
        ])
        .expect("parse Temporal connection flags");
        let config = cli
            .run
            .temporal_settings(None)
            .resolve()
            .expect("consistent Temporal settings");

        assert_eq!(config.target().as_str(), "https://temporal.internal:7233/");
        assert_eq!(config.namespace(), "lightspeed");
        let tls = config.tls().expect("TLS is on");
        assert_eq!(
            tls.server_ca_file.as_deref(),
            Some(std::path::Path::new("/etc/temporal/ca.pem"))
        );
        assert_eq!(
            tls.server_name.as_deref(),
            Some("frontend.temporal.internal")
        );
        assert!(config.uses_client_certificate());
        assert!(config.uses_api_key());
    }

    #[test]
    fn inconsistent_temporal_settings_fail_before_connecting() {
        // Every flag is given, blank where unset, so the variables of the
        // shell that runs the test cannot leak into the parse.
        let parse = |tls: &str, cert: &str, key: &str| {
            Cli::try_parse_from([
                "lightspeed-runtime",
                "--temporal-target",
                "temporal.internal:7233",
                "--namespace",
                "lightspeed",
                "--temporal-tls",
                tls,
                "--temporal-tls-server-ca-file",
                "",
                "--temporal-tls-server-name",
                "",
                "--temporal-tls-client-cert-file",
                cert,
                "--temporal-tls-client-key-file",
                key,
                "--temporal-api-key-file",
                "",
            ])
            .expect("parse Temporal connection flags")
            .run
        };

        assert!(matches!(
            parse("", "/etc/temporal/client.pem", "")
                .temporal_settings(None)
                .resolve(),
            Err(TemporalConfigError::IncompleteClientCertificate { .. })
        ));
        assert!(matches!(
            parse("sometimes", "", "").temporal_settings(None).resolve(),
            Err(TemporalConfigError::InvalidTlsSwitch)
        ));
        assert!(matches!(
            parse("", "", "")
                .temporal_settings(Some("key-1".to_owned()))
                .resolve(),
            Ok(config) if config.tls().is_some() && config.uses_api_key()
        ));
        let plain = parse("", "", "")
            .temporal_settings(None)
            .resolve()
            .expect("no TLS setting");
        assert!(plain.tls().is_none());
        assert_eq!(plain.target().as_str(), "http://temporal.internal:7233/");
    }

    #[test]
    fn registration_urls_are_a_comma_separated_list() {
        let cli = Cli::try_parse_from([
            "lightspeed-runtime",
            "--environment-registration-urls",
            "ws://gateway.internal:18080,wss://gateway.example",
        ])
        .expect("parse registration URLs");
        assert_eq!(
            cli.run.environment_registration_urls,
            ["ws://gateway.internal:18080", "wss://gateway.example"]
        );
    }

    #[test]
    fn code_role_accepts_its_own_queue_and_interpreter_capacity() {
        let cli = Cli::try_parse_from([
            "lightspeed-runtime",
            "--roles",
            "code",
            "--task-queue",
            "test-sessions",
            "--code-task-queue",
            "test-code",
            "--code-max-concurrent-executions",
            "7",
        ])
        .expect("parse code role configuration");
        assert_eq!(cli.run.roles().unwrap().to_string(), "code");
        let queues = cli.run.task_queues().expect("deployment queues");
        assert_eq!(queues.sessions, "test-sessions");
        assert_eq!(queues.code, "test-code");
        assert_eq!(cli.run.code_max_concurrent_executions, 7);
    }

    #[test]
    fn changed_migration_explains_safe_recovery_without_hashes() {
        let message = explain_migration_error(store_pg::PgStoreError::MigrationChecksumChanged {
            version: 1,
            name: "core",
            expected: "expected-hash".into(),
            actual: "recorded-hash".into(),
        })
        .to_string();
        assert!(message.contains("migration 1 (core)"));
        assert!(message.contains("./dev.sh reset"));
        assert!(message.contains("deletes local PostgreSQL and MinIO data"));
        assert!(message.contains("To keep the data"));
        assert!(!message.contains("expected-hash"));
        assert!(!message.contains("recorded-hash"));

        let renamed = explain_migration_error(store_pg::PgStoreError::MigrationNameChanged {
            version: 1,
            expected: "core",
            actual: "old_core".into(),
        })
        .to_string();
        assert!(renamed.contains("migration 1 (core) has a different name"));
        assert!(renamed.contains("./dev.sh reset"));
    }

    #[test]
    fn universe_creation_needs_no_identity_and_key_commands_are_removed() {
        assert!(Cli::try_parse_from(["server", "universe", "create"]).is_ok());
        assert!(Cli::try_parse_from(["server", "api-key", "create"]).is_err());
    }
}
