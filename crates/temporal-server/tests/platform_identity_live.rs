//! Platform login, directory administration and user-attributed runtime requests.
mod support;

use access::*;
use auth::{ApiKeyStore as _, CreateApiKey};
use std::sync::Arc;
use store_pg::{PgAccessStore, PgApiKeyStore, PgStore};
use temporal_server::{
    DeploymentStores, UniverseRuntime,
    config::GatewayAuthMode,
    gateway::{GatewayRoutes, GatewayState, gateway_router},
};
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires disposable PostgreSQL/Temporal and built npm client; serialized; no provider credentials"]
async fn platform_accounts_use_canonical_access_end_to_end() -> anyhow::Result<()> {
    let _lock = support::live::LIVE_TEST_LOCK.lock().await;
    let url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")?;
    anyhow::ensure!(url == std::env::var("LIGHTSPEED_POSTGRES_URL")?);
    let pool = sqlx::PgPool::connect(&url).await?;
    PgStore::migrate(&pool).await?;
    let universe = support::live::live_universe_id()?;
    store_pg::create_universe(&pool, universe).await?;
    let store = PgAccessStore::new(pool.clone());
    let host = store.initialize_local_development(universe, 1).await?;
    let admin = Uuid::new_v4();
    let service = Uuid::new_v4();
    for (id, kind) in [
        (admin, PrincipalKind::User),
        (service, PrincipalKind::Service),
    ] {
        store
            .apply(
                host.id,
                AccessChange::CreatePrincipal {
                    id,
                    kind,
                    display_name: format!("Platform test {kind:?}"),
                    management_scope: AccessScope::Deployment,
                },
                2,
            )
            .await?;
    }
    store
        .apply(
            host.id,
            AccessChange::AssignRole {
                assignment: RoleAssignment {
                    scope: AccessScope::Deployment,
                    subject: Subject::Principal(admin),
                    role: Role::DeploymentAdmin,
                },
            },
            3,
        )
        .await?;
    for capability in [
        ServiceCapability::AssertUser,
        ServiceCapability::ManageIdentity,
    ] {
        store
            .apply(
                host.id,
                AccessChange::AssignCapability {
                    assignment: CapabilityAssignment {
                        scope: AccessScope::Deployment,
                        principal_id: service,
                        capability,
                    },
                },
                4,
            )
            .await?;
    }
    let key = auth::mint_api_key(AccessScope::Deployment, service, host.id, None, 5);
    PgApiKeyStore::new(pool.clone())
        .create_api_key(CreateApiKey {
            authority_scope: AccessScope::Deployment,
            key_hash: key.key_hash,
            record: key.record,
        })
        .await?;
    let activities = support::live::fake_worker_activities().await?;
    support::live::run_with_live_worker(activities, move |client, queue, _| async move {
        let runtime = Arc::new(UniverseRuntime::new(client, queue, None, DeploymentStores::from_env().await?)?);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/rpc", listener.local_addr()?);
        let router = gateway_router(Arc::new(GatewayState::multi(GatewayAuthMode::Authenticated, runtime, endpoint.clone())), 1024*1024, GatewayRoutes { api: true, environment: false });
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let result = tokio::time::timeout(std::time::Duration::from_secs(180), tokio::process::Command::new("node")
            .current_dir(&root).args(["--import", "tsx", "platform/server/test/identity-live.ts"])
            .env("LIGHTSPEED_TEST_API_URL", endpoint)
            .env("LIGHTSPEED_TEST_UNIVERSE_ID", universe.to_string())
            .env("LIGHTSPEED_TEST_PLATFORM_KEY", key.secret.expose())
            .env("LIGHTSPEED_TEST_ADMIN_PRINCIPAL", admin.to_string())
            .kill_on_drop(true).output()).await;
        server.abort();
        let output = result??;
        anyhow::ensure!(output.status.success(), "Platform test failed:\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8(output.stdout)?;
        let summary: serde_json::Value = serde_json::from_str(stdout.lines().last().unwrap())?;
        let universe = Uuid::parse_str(summary["universe"].as_str().unwrap())?;
        for (index, user) in ["alice", "bob"].iter().enumerate() {
            let id = Uuid::parse_str(summary[user].as_str().unwrap())?;
            let session = summary["sessions"][index].as_str().unwrap();
            let anchor = store.anchor(universe, &ResourceRef::Session(session.into())).await?.unwrap();
            assert_eq!(anchor.created_by, ActionActor::Principal { id });
            assert_eq!(anchor.controller, ResourceController::Principal(id));
        }
        let assertions: i64 = sqlx::query_scalar("SELECT count(*) FROM access_audit_events WHERE universe_id=$1 AND authenticated_principal_id=$2")
            .bind(universe).bind(service).fetch_one(&pool).await?;
        assert!(assertions >= 2, "missing authenticated service attribution");
        println!("Platform identity live: {} HTTP checks; {assertions} attributed events", summary["checks"]);
        Ok(())
    }).await
}
