//! Exercise three distinct gateway roles through one deterministic load
//! balancer, including daemon reconnection when its owner drains.

mod support;

use std::{
    net::SocketAddr,
    str::FromStr as _,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use api::{AgentApiService, EnvironmentIdentityModeView, EnvironmentRegistrationKeyCreateParams};
use environment_daemon::{
    DaemonRuntime,
    config::{DaemonConfig, RegistrationConfig},
};
use environment_protocol::{bytes::SecretString, data::idle::GetIdleParams};
use sqlx::{
    Executor as _,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::{PgStore, PgStoreConfig};
use temporal_runtime::{
    environments::gateway::{EnvironmentGatewayClientConfig, RouteKey},
    gateway::{
        DEFAULT_MAX_REQUEST_BODY_BYTES, GatewayAgentApi, GatewayRoutes, GatewayState,
        gateway_router,
        replica::GatewayReplica,
        request_context::{RequestContext, with_request_context},
    },
};
use temporal_workflow::{TemporalConnectionConfig, connect_temporal};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest as _, http::HeaderValue},
};
use uuid::Uuid;

struct Tasks(Vec<tokio::task::AbortHandle>);

impl Tasks {
    fn spawn<F>(&mut self, future: F)
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.0.push(tokio::spawn(future).abort_handle());
    }
}

impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + PostgreSQL"]
async fn distinct_owner_acceptor_receiver_route_and_handover() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    support::live::require_storage_live_env()?;
    let _guard = support::live::LIVE_TEST_LOCK.lock().await;
    let database = Database::new().await?;
    let outcome =
        tokio::time::timeout(Duration::from_secs(90), scenario(database.pool.clone())).await;
    database.close().await?;
    outcome??;
    Ok(())
}

struct Database {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
}

impl Database {
    async fn new() -> anyhow::Result<Self> {
        let url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")?;
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await?;
        let schema = format!("gateway_replicas_test_{}", Uuid::new_v4().simple());
        admin
            .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "CREATE SCHEMA \"{schema}\""
            ))))
            .await?;
        let options = PgConnectOptions::from_str(&url)?
            .application_name(&schema)
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(12)
            .connect_with(options)
            .await?;
        let database = Self {
            admin,
            pool,
            schema,
        };
        if let Err(error) = PgStore::migrate(&database.pool).await {
            database.close().await?;
            return Err(error.into());
        }
        Ok(database)
    }

    async fn close(self) -> anyhow::Result<()> {
        self.pool.close().await;
        self.admin
            .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA \"{}\" CASCADE",
                self.schema
            ))))
            .await?;
        self.admin.close().await;
        Ok(())
    }
}

async fn scenario(pool: sqlx::PgPool) -> anyhow::Result<()> {
    let universe_id = Uuid::new_v4();
    let store = Arc::new(PgStore::new(pool.clone(), PgStoreConfig::new(universe_id)));
    store.ensure_universe().await?;
    let temporal = connect_temporal(&TemporalConnectionConfig::from_env()?).await?;
    let public = TcpListener::bind("127.0.0.1:0").await?;
    let public_url = format!("http://{}", public.local_addr()?);
    let token = format!("replicas-{}", Uuid::new_v4());
    let worker = EnvironmentGatewayClientConfig::new(&public_url, &token);
    let mut tasks = Tasks(Vec::new());
    let mut addresses = Vec::new();
    let mut states = Vec::new();
    let mut apis = Vec::new();
    for _ in 0..3 {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        addresses.push(address);
        let api = Arc::new(
            GatewayAgentApi::builder(temporal.clone(), store.clone())
                .with_public_base_url(&public_url)
                .with_environment_gateway(worker.clone())
                .build(),
        );
        let replica = GatewayReplica::register(
            &pool,
            &["environment-gateway".to_owned()],
            Some(&format!("http://{address}")),
        )
        .await?;
        let state = Arc::new(GatewayState::for_api(api.clone()).with_replica(replica.clone()));
        tasks.spawn(replica.maintain(state.clone()));
        let router = gateway_router(
            state.clone(),
            DEFAULT_MAX_REQUEST_BODY_BYTES,
            GatewayRoutes {
                api: false,
                environment: true,
            },
        );
        tasks.spawn(async move { axum::serve(listener, router).await });
        states.push(state);
        apis.push(api);
    }
    let owner_target = Arc::new(AtomicUsize::new(0));
    let receiver_target = Arc::new(AtomicUsize::new(2));
    tasks.spawn(balance(
        public,
        addresses.clone(),
        owner_target.clone(),
        receiver_target.clone(),
    ));
    let minted = with_request_context(
        RequestContext::local(api::AccessScope::Universe { universe_id }),
        apis[0].create_environment_registration_key(EnvironmentRegistrationKeyCreateParams {
            registration_key_id: None,
            display_name: "replica integration".to_owned(),
            identity_mode: EnvironmentIdentityModeView::Persistent,
            max_active_environments: Some(1),
            ephemeral_disconnect_grace_ms: None,
            offline_close_after_ms: None,
            declared_only: false,
            expires_at_ms: None,
        }),
    )
    .await?
    .result;
    let sandbox = tempfile::tempdir()?;
    let root = sandbox.path().join("workspace");
    std::fs::create_dir(&root)?;
    let root = root.canonicalize()?;
    let connect_url = public_url.replace("http://", "ws://") + "/environment-gateway/connect";
    let mut registration = RegistrationConfig::new(
        connect_url.clone(),
        environment_daemon::upgrade::resolve_discovery_url(Some(&connect_url), None)?,
    );
    registration.registration_key = Some(SecretString::new(minted.secret.0));
    let daemon = DaemonRuntime::new(DaemonConfig {
        listen: None,
        cwd: root.clone(),
        fs_root: root,
        state_dir: sandbox.path().join("state"),
        read_only_fs: false,
        registration: Some(registration),
        scrubbed_env: Vec::new(),
    })?;
    tasks.spawn(environment_daemon::server::run(daemon.clone()));
    let environment_id =
        wait_for_owner(&pool, universe_id, states[0].replica().instance_id()).await?;
    let key = RouteKey {
        universe_id,
        environment_id: environment_id.clone(),
    };

    // Worker -> replica 1, control -> replica 0, data -> replica 2.
    let (client, _) = worker
        .connect(&key, worker.connect_options("replica-integration"))
        .await?;
    client.get_idle(&GetIdleParams {}).await?;
    client.close().await?;

    // A later gateway startup must leave the existing owner's row intact.
    let extra = GatewayReplica::register(&pool, &["environment-gateway".to_owned()], None).await?;
    assert_eq!(
        store_pg::read_registered_connection_owner(&pool, universe_id, &environment_id)
            .await?
            .expect("connected")
            .owner_instance_id,
        states[0].replica().instance_id()
    );
    extra.unregister(&pool).await;

    // Receiver = acceptor exercises the single-process data path while
    // the owner remains a distinct gateway.
    receiver_target.store(1, Ordering::Release);
    let (client, _) = worker
        .connect(&key, worker.connect_options("replica-local-pairing"))
        .await?;
    client.get_idle(&GetIdleParams {}).await?;
    client.close().await?;

    // Neither a forged public token nor an unauthenticated peer handoff
    // can open a stream.
    let mut invalid =
        format!("ws://{}/environment-gateway/data", addresses[2]).into_client_request()?;
    invalid.headers_mut().insert(
        "authorization",
        HeaderValue::from_static("Bearer invalid.mac"),
    );
    let error = connect_async(invalid)
        .await
        .expect_err("forged token must fail");
    assert!(
        matches!(error, tokio_tungstenite::tungstenite::Error::Http(ref response) if response.status().as_u16() == 401)
    );
    let error = connect_async(format!(
        "ws://{}/environment-gateway/peer/data",
        addresses[1]
    ))
    .await
    .expect_err("peer credential is required");
    assert!(
        matches!(error, tokio_tungstenite::tungstenite::Error::Http(ref response) if response.status().as_u16() == 401)
    );

    // Ordinary control closure during bounded drain must reconnect the
    // same daemon, rather than deliver terminal registration/closed.
    owner_target.store(1, Ordering::Release);
    states[0].replica().drain(&states[0]).await;
    let reconnected = wait_for_owner(&pool, universe_id, states[1].replica().instance_id()).await?;
    assert_eq!(reconnected, environment_id);
    receiver_target.store(2, Ordering::Release);
    let (client, _) = worker
        .connect(&key, worker.connect_options("replica-after-handover"))
        .await?;
    client.get_idle(&GetIdleParams {}).await?;
    client.close().await?;
    daemon.shutdown().cancel();
    for state in &states {
        state.replica().unregister(&pool).await;
    }
    Ok(())
}

async fn wait_for_owner(
    pool: &sqlx::PgPool,
    universe: Uuid,
    owner: Uuid,
) -> anyhow::Result<String> {
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let id: Option<String> = sqlx::query_scalar("SELECT environment_id FROM environments WHERE universe_id = $1 AND owner_instance_id = $2 AND connected")
                .bind(universe).bind(owner).fetch_optional(pool).await?;
            if let Some(id) = id { return Ok(id); }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await?
}

/// Forward the untouched HTTP upgrade and subsequent bytes. The public
/// origin stays constant while each route family deliberately lands on a
/// different backend.
async fn balance(
    listener: TcpListener,
    backends: Vec<SocketAddr>,
    owner: Arc<AtomicUsize>,
    receiver: Arc<AtomicUsize>,
) -> anyhow::Result<()> {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let (mut downstream, _) = listener.accept().await?;
        let backends = backends.clone();
        let owner = owner.clone();
        let receiver = receiver.clone();
        connections.spawn(async move {
            let mut headers = Vec::new();
            loop {
                let byte = downstream.read_u8().await?;
                headers.push(byte);
                anyhow::ensure!(headers.len() <= 16384, "oversized test HTTP headers");
                if headers.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&headers);
            let index = if request.starts_with("GET /environment-gateway/connect ") {
                owner.load(Ordering::Acquire)
            } else if request.starts_with("GET /environment-gateway/data ") {
                receiver.load(Ordering::Acquire)
            } else {
                1
            };
            let mut upstream = TcpStream::connect(backends[index]).await?;
            upstream.write_all(&headers).await?;
            tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await?;
            Ok::<_, anyhow::Error>(())
        });
        while connections.try_join_next().is_some() {}
    }
}
