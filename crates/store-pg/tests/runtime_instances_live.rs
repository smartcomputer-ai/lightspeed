//! PostgreSQL concurrency contracts for gateway leases and ownership.

use std::{str::FromStr as _, time::Duration};

use environments::{
    CreateEnvironmentRegistrationKey, CreateRegisteredEnvironment, DaemonBuild, EnvironmentId,
    EnvironmentRegistrationKeyId, EnvironmentRegistrationKeyStore, EnvironmentRegistryError,
    EnvironmentState, EnvironmentStore, RegisteredIdentityMode, RegistrationKeyPolicy,
    mint_registration_key,
};
use sqlx::{
    Executor as _, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use store_pg::{
    PgStore, PgStoreConfig, RuntimeLoop, RuntimeLoopLeader, delete_runtime_instance,
    disconnect_runtime_instance_connections, read_live_runtime_instance,
    read_registered_connection_owners, register_runtime_instance, renew_runtime_instance,
    sweep_expired_registered_connections,
};
use uuid::Uuid;

const LEASE: Duration = Duration::from_secs(60);

struct Database {
    admin: PgPool,
    pool: PgPool,
    schema: String,
    store: PgStore,
}

impl Database {
    async fn new() -> Self {
        let url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")
            .expect("LIGHTSPEED_TEST_POSTGRES_URL must be set; source scripts/dev/env.sh");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let schema = format!("gateway_registry_test_{}", Uuid::new_v4().simple());
        admin
            .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "CREATE SCHEMA \"{schema}\""
            ))))
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&url)
            .unwrap()
            .application_name(&schema)
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .unwrap();
        PgStore::migrate(&pool).await.unwrap();
        let store = PgStore::new(pool.clone(), PgStoreConfig::new(Uuid::new_v4()));
        store.ensure_universe().await.unwrap();
        Self {
            admin,
            pool,
            schema,
            store,
        }
    }

    async fn close(self) {
        self.pool.close().await;
        self.admin
            .execute(sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA \"{}\" CASCADE",
                self.schema
            ))))
            .await
            .unwrap();
        self.admin.close().await;
    }

    async fn request(
        &self,
        key: &str,
        mode: RegisteredIdentityMode,
        capacity: u32,
    ) -> CreateRegisteredEnvironment {
        let minted = mint_registration_key(
            EnvironmentRegistrationKeyId::new(key),
            RegistrationKeyPolicy {
                display_name: key.to_owned(),
                identity_mode: mode,
                max_active_environments: Some(capacity),
                ephemeral_disconnect_grace_ms: None,
                offline_close_after_ms: None,
                declared_only: false,
                expires_at_ms: None,
            },
            1,
        )
        .unwrap();
        self.store
            .create_registration_key(CreateEnvironmentRegistrationKey {
                record: minted.record,
                secret_hash: minted.secret.secret_hash.clone(),
            })
            .await
            .unwrap();
        let now_ms: i64 =
            sqlx::query_scalar("SELECT (extract(epoch FROM clock_timestamp()) * 1000)::bigint")
                .fetch_one(&self.pool)
                .await
                .unwrap();
        CreateRegisteredEnvironment {
            environment_id: EnvironmentId::new(format!("environment-{key}")),
            registration_key_id: EnvironmentRegistrationKeyId::new(key),
            secret_hash: minted.secret.secret_hash,
            daemon_public_key: Uuid::new_v4().simple().to_string().repeat(2),
            display_name: None,
            metadata: Default::default(),
            daemon_build: build(),
            created_at_ms: now_ms,
        }
    }

    async fn register(&self, id: Uuid) {
        register_runtime_instance(
            &self.pool,
            id,
            &["environment-gateway".to_owned()],
            Some(&format!("http://{id}.internal:8080")),
            LEASE,
        )
        .await
        .unwrap();
    }

    async fn expire(&self, id: Uuid) {
        sqlx::query("UPDATE runtime_instances SET started_at_ms = 0, lease_expires_at_ms = 0 WHERE instance_id = $1")
            .bind(id).execute(&self.pool).await.unwrap();
    }

    async fn wait_for_blocked_queries(&self, query_part: &str, count: i64) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let waiting: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM pg_stat_activity WHERE application_name = $1 AND wait_event_type = 'Lock' AND position($2 in query) > 0"
                ).bind(&self.schema).bind(query_part).fetch_one(&self.pool).await.unwrap();
                if waiting >= count { break; }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.expect("queries must reach their PostgreSQL row lock");
    }
}

fn build() -> DaemonBuild {
    DaemonBuild {
        version: Some("test".to_owned()),
        git_sha: None,
        protocol_version: environment_protocol::ENVD_PROTOCOL_VERSION,
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires PostgreSQL; source scripts/dev/env.sh"]
async fn ownership_fences_stale_writes_and_expired_leases() {
    let db = Database::new().await;
    let request = db
        .request("ephemeral", RegisteredIdentityMode::Ephemeral, 1)
        .await;
    let environment = db
        .store
        .create_registered_environment(request.clone())
        .await
        .unwrap();
    let id = &environment.environment_id;
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    db.register(a).await;
    assert!(
        db.store
            .claim_registered_connection(
                id,
                a,
                "connection-a",
                &build(),
                &request.daemon_public_key
            )
            .await
            .unwrap()
    );
    // Starting another replica neither repairs nor disconnects live owners.
    db.register(b).await;
    assert_eq!(
        db.store
            .read_registered_connection_owner(id)
            .await
            .unwrap()
            .unwrap()
            .owner_instance_id,
        a
    );
    assert!(
        !db.store
            .claim_registered_connection(id, b, "wrong-identity", &build(), &"ff".repeat(32))
            .await
            .unwrap()
    );
    assert!(
        db.store
            .claim_registered_connection(
                id,
                b,
                "connection-b",
                &build(),
                &request.daemon_public_key
            )
            .await
            .unwrap()
    );
    assert!(
        !db.store
            .stamp_owned_registered_connection(id, a, "connection-a")
            .await
            .unwrap()
    );
    assert!(
        !db.store
            .disconnect_owned_registered_connection(id, a, "connection-a")
            .await
            .unwrap()
    );
    assert!(
        !db.store
            .depart_owned_registered_connection(id, a, "connection-a")
            .await
            .unwrap()
    );
    assert_eq!(
        disconnect_runtime_instance_connections(&db.pool, a)
            .await
            .unwrap(),
        0
    );
    let owners = read_registered_connection_owners(
        &db.pool,
        &[(db.store.config().universe_id, id.to_string())],
    )
    .await
    .unwrap();
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0].2.control_connection_id, "connection-b");
    db.expire(a).await;
    assert!(!renew_runtime_instance(&db.pool, a, LEASE).await.unwrap());
    assert!(
        !db.store
            .claim_registered_connection(
                id,
                a,
                "connection-expired",
                &build(),
                &request.daemon_public_key
            )
            .await
            .unwrap()
    );
    assert_eq!(
        sweep_expired_registered_connections(&db.pool)
            .await
            .unwrap(),
        0
    );
    assert!(
        db.store
            .stamp_owned_registered_connection(id, b, "connection-b")
            .await
            .unwrap()
    );
    assert!(
        db.store
            .depart_owned_registered_connection(id, b, "connection-b")
            .await
            .unwrap()
    );
    assert_eq!(
        db.store.read_environment(id).await.unwrap().state,
        EnvironmentState::Closed
    );
    assert!(
        db.store
            .read_registered_connection_owner(id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !db.store
            .claim_registered_connection(
                id,
                b,
                "connection-after-close",
                &build(),
                &request.daemon_public_key
            )
            .await
            .unwrap()
    );
    delete_runtime_instance(&db.pool, b).await.unwrap();
    db.close().await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires PostgreSQL; source scripts/dev/env.sh"]
async fn lease_expiry_disconnects_only_its_owners_and_removes_dead_instances() {
    let db = Database::new().await;
    let request = db
        .request("persistent", RegisteredIdentityMode::Persistent, 1)
        .await;
    let environment = db
        .store
        .create_registered_environment(request.clone())
        .await
        .unwrap();
    // A just-created row may still be awaiting its ownership claim.
    assert_eq!(
        sweep_expired_registered_connections(&db.pool)
            .await
            .unwrap(),
        0
    );
    let a = Uuid::new_v4();
    db.register(a).await;
    let instance = read_live_runtime_instance(&db.pool, a)
        .await
        .unwrap()
        .unwrap();
    assert!(instance.lease_expires_at_ms > instance.started_at_ms);
    assert!(renew_runtime_instance(&db.pool, a, LEASE).await.unwrap());
    assert!(
        db.store
            .claim_registered_connection(
                &environment.environment_id,
                a,
                "connection",
                &build(),
                &request.daemon_public_key
            )
            .await
            .unwrap()
    );
    db.expire(a).await;
    assert!(
        read_live_runtime_instance(&db.pool, a)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !db.store
            .stamp_owned_registered_connection(&environment.environment_id, a, "connection")
            .await
            .unwrap()
    );
    assert_eq!(
        sweep_expired_registered_connections(&db.pool)
            .await
            .unwrap(),
        1
    );
    assert!(
        !db.store
            .read_environment(&environment.environment_id)
            .await
            .unwrap()
            .registered()
            .unwrap()
            .connected
    );
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM runtime_instances")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    assert!(!renew_runtime_instance(&db.pool, a, LEASE).await.unwrap());
    db.close().await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires PostgreSQL; source scripts/dev/env.sh"]
async fn expiry_sweep_waiting_on_a_row_cannot_clear_a_new_instance_takeover() {
    let db = Database::new().await;
    let request = db
        .request("takeover", RegisteredIdentityMode::Persistent, 1)
        .await;
    let environment = db
        .store
        .create_registered_environment(request.clone())
        .await
        .unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    db.register(a).await;
    assert!(
        db.store
            .claim_registered_connection(
                &environment.environment_id,
                a,
                "old",
                &build(),
                &request.daemon_public_key
            )
            .await
            .unwrap()
    );
    db.expire(a).await;
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query("SELECT environment_id FROM environments WHERE universe_id = $1 AND environment_id = $2 FOR UPDATE")
        .bind(db.store.config().universe_id).bind(environment.environment_id.as_str())
        .execute(&mut *tx).await.unwrap();
    let pool = db.pool.clone();
    let sweep = tokio::spawn(async move { sweep_expired_registered_connections(&pool).await });
    db.wait_for_blocked_queries("WITH expired AS MATERIALIZED", 1)
        .await;
    // This new instance is absent from the waiting sweep's statement snapshot.
    db.register(b).await;
    sqlx::query("UPDATE environments SET owner_instance_id = $1, control_connection_id = 'new' WHERE universe_id = $2 AND environment_id = $3")
        .bind(b).bind(db.store.config().universe_id).bind(environment.environment_id.as_str())
        .execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(sweep.await.unwrap().unwrap(), 0);
    assert_eq!(
        db.store
            .read_registered_connection_owner(&environment.environment_id)
            .await
            .unwrap()
            .unwrap()
            .owner_instance_id,
        b
    );
    db.close().await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires PostgreSQL; source scripts/dev/env.sh"]
async fn concurrent_admissions_of_one_identity_share_the_last_capacity_slot() {
    let db = Database::new().await;
    let request = db
        .request("capacity", RegisteredIdentityMode::Persistent, 1)
        .await;
    let mut lock = db.pool.begin().await.unwrap();
    sqlx::query("SELECT registration_key_id FROM environment_registration_keys WHERE universe_id = $1 AND registration_key_id = $2 FOR UPDATE")
        .bind(db.store.config().universe_id).bind(request.registration_key_id.as_str())
        .execute(&mut *lock).await.unwrap();
    let left_store = db.store.clone();
    let left_request = request.clone();
    let left =
        tokio::spawn(async move { left_store.create_registered_environment(left_request).await });
    let right_store = db.store.clone();
    let mut right_request = request.clone();
    right_request.environment_id = EnvironmentId::new("other-proposed-id");
    let right = tokio::spawn(async move {
        right_store
            .create_registered_environment(right_request)
            .await
    });
    db.wait_for_blocked_queries("FOR UPDATE", 2).await;
    lock.commit().await.unwrap();
    let left = left.await.unwrap().unwrap();
    let right = right.await.unwrap().unwrap();
    assert_eq!(left.environment_id, right.environment_id);
    let mut extra = request;
    extra.environment_id = EnvironmentId::new("over-capacity");
    extra.daemon_public_key = Uuid::new_v4().simple().to_string().repeat(2);
    assert!(matches!(
        db.store.create_registered_environment(extra).await,
        Err(EnvironmentRegistryError::RegistrationCapacityExhausted { limit: 1, .. })
    ));
    db.close().await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires PostgreSQL; source scripts/dev/env.sh"]
async fn loop_leadership_is_independent_and_transfers_after_session_close() {
    let db = Database::new().await;
    let mut leader = RuntimeLoopLeader::try_acquire(&db.pool, RuntimeLoop::EnvironmentLifecycle)
        .await
        .unwrap()
        .unwrap();
    leader.check().await.unwrap();
    assert!(
        RuntimeLoopLeader::try_acquire(&db.pool, RuntimeLoop::EnvironmentLifecycle)
            .await
            .unwrap()
            .is_none()
    );
    let sweep = RuntimeLoopLeader::try_acquire(&db.pool, RuntimeLoop::RegisteredSweep)
        .await
        .unwrap()
        .unwrap();
    let display = RuntimeLoopLeader::try_acquire(&db.pool, RuntimeLoop::EnvironmentDisplaySweep)
        .await
        .unwrap()
        .unwrap();
    drop(leader);
    let successor = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(leader) =
                RuntimeLoopLeader::try_acquire(&db.pool, RuntimeLoop::EnvironmentLifecycle)
                    .await
                    .unwrap()
            {
                break leader;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("closing the leader session releases its lock");
    drop((successor, sweep, display));
    db.close().await;
}
