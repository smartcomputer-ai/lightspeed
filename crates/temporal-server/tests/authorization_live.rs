//! Authenticated HTTP and direct-service authorization over real PostgreSQL and
//! Temporal, with a fake model. Requires an explicitly selected disposable DB.
mod support;

use access::*;
use api::{AgentApiErrorKind, AgentApiService as _};
use auth::{ApiKeyStore as _, CreateApiKey, MintedApiKey};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use store_pg::{PgAccessStore, PgApiKeyStore, PgStore};
use temporal_server::{
    DeploymentStores, UniverseRuntime,
    config::GatewayAuthMode,
    gateway::{GatewayRoutes, GatewayState, gateway_router, principal::with_request_context},
};
use uuid::Uuid;

async fn rpc(url: &str, key: &MintedApiKey, method: &str, params: Value) -> Value {
    reqwest::Client::new()
        .post(url)
        .bearer_auth(key.secret.expose())
        .json(&json!({"id":1,"method":method,"params":params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
fn success(value: Value) -> Value {
    assert!(value.get("error").is_none(), "{value}");
    value["result"]["result"].clone()
}
fn rejected(value: Value) {
    assert_eq!(value["error"]["data"]["kind"], "rejected", "{value}");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires disposable Postgres and Temporal; fake model; serialized"]
async fn authenticated_roles_ownership_and_direct_service_boundaries() -> anyhow::Result<()> {
    let _lock = support::live::LIVE_TEST_LOCK.lock().await;
    let url = std::env::var("LIGHTSPEED_TEST_POSTGRES_URL")?;
    assert_eq!(
        std::env::var("LIGHTSPEED_POSTGRES_URL")?,
        url,
        "select the same disposable database for runtime and test"
    );
    let pool = sqlx::PgPool::connect(&url).await?;
    PgStore::migrate(&pool).await?;
    let universe = support::live::live_universe_id()?;
    PgStore::new(pool.clone(), store_pg::PgStoreConfig::new(universe))
        .ensure_universe()
        .await?;
    let access = PgAccessStore::new(pool.clone());
    // The host-only development fixture is explicit; HTTP still uses scoped keys.
    let admin = access.initialize_local_development(universe, 1).await?;
    let keys = PgApiKeyStore::new(pool.clone());
    let scope = AccessScope::Universe {
        universe_id: universe,
    };
    let mut callers = Vec::new();
    for role in [
        Role::Viewer,
        Role::Contributor,
        Role::Contributor,
        Role::Operator,
        Role::Admin,
    ] {
        let id = Uuid::new_v4();
        access
            .apply(
                admin.id,
                AccessChange::CreatePrincipal {
                    id,
                    kind: PrincipalKind::User,
                    management_scope: AccessScope::Deployment,
                    display_name: format!("Test {role:?}"),
                },
                2,
            )
            .await?;
        access
            .apply(
                admin.id,
                AccessChange::AssignRole {
                    assignment: RoleAssignment {
                        scope,
                        subject: Subject::Principal(id),
                        role,
                    },
                },
                3,
            )
            .await?;
        let key = auth::mint_api_key(scope, id, admin.id, None, 4);
        keys.create_api_key(CreateApiKey {
            authority_scope: AccessScope::Deployment,
            key_hash: key.key_hash.clone(),
            record: key.record.clone(),
        })
        .await?;
        callers.push(key);
    }
    let activities = support::live::fake_worker_activities().await?;
    support::live::run_with_live_worker(activities, move |client, queue, session_id| async move {
        let runtime = Arc::new(UniverseRuntime::new(client, queue, None, DeploymentStores::from_env().await?)?);
        let api = runtime.state_for(universe, false).await?.api.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/rpc", listener.local_addr()?);
        let router = gateway_router(Arc::new(GatewayState::multi(GatewayAuthMode::Authenticated,
            runtime, endpoint.clone())), 1024 * 1024, GatewayRoutes { api: true, environment: false });
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let outcome = async {
            let [viewer, alice, bob, operator, universe_admin] = callers.as_slice() else { unreachable!() };
            // Every declared mutating/service route rejects a Viewer before decoding its body.
            for method in api::method_manifest() {
                if !matches!(method.access, api::MethodAccess::Universe(UniverseAction::Read)) {
                    rejected(rpc(&endpoint, viewer, method.method, json!({})).await);
                }
            }
            let session = session_id.as_str();
            let start = json!({"sessionId":session});
            rejected(rpc(&endpoint, viewer, "session/start", start.clone()).await);
            success(rpc(&endpoint, alice, "session/start", start.clone()).await);
            success(rpc(&endpoint, alice, "session/start", start.clone()).await);
            for caller in [bob, operator, universe_admin] {
                rejected(rpc(&endpoint, caller, "session/start", start.clone()).await);
                rejected(rpc(&endpoint, caller, "session/rename", json!({"sessionId":session,"displayName":"takeover"})).await);
                rejected(rpc(&endpoint, caller, "session/runs/start", json!({"sessionId":session,"source":{"type":"input","items":[{"type":"text","text":"takeover"}]}})).await);
                rejected(rpc(&endpoint, caller, "session/context/append", json!({"sessionId":session,"entries":[]})).await);
            }
            success(rpc(&endpoint, viewer, "session/read", json!({"sessionId":session})).await);
            success(rpc(&endpoint, viewer, "session/list", json!({})).await);
            rejected(rpc(&endpoint, viewer, "blobs/put", json!({"blobs":[]})).await);
            // A contributor can author templates but cannot edit another author's template.
            let profile = format!("profile-{}", Uuid::new_v4().simple());
            let document = json!({"profile":{"profileId":profile}});
            success(rpc(&endpoint, alice, "profiles/put", document.clone()).await);
            rejected(rpc(&endpoint, bob, "profiles/put", document.clone()).await);
            rejected(rpc(&endpoint, viewer, "profiles/put", document.clone()).await);
            success(rpc(&endpoint, operator, "profiles/put", document.clone()).await);
            let owned = access.ownership(universe, &ResourceRef::Profile(profile.clone())).await?.unwrap();
            assert_eq!(owned.created_by, ActionActor::Principal { id: alice.record.principal_id });
            assert_eq!(owned.controller, ResourceController::Principal(alice.record.principal_id));
            let bot = format!("bot-{}", Uuid::new_v4().simple());
            let create_bot = json!({"bot":{"botId":bot,"profileId":profile},
                "triggers":[{"triggerId":"hook","kind":"webhook"}]});
            success(rpc(&endpoint, alice, "bots/create", create_bot).await);
            let bot_ownership = access.ownership(universe, &ResourceRef::Bot(bot.clone())).await?.unwrap();
            assert_eq!(bot_ownership.created_by, ActionActor::Principal { id: alice.record.principal_id });
            let trigger = json!({"botId":bot,"triggerId":"hook"});
            for caller in [alice, operator] {
                assert!(success(rpc(&endpoint, caller, "bots/triggers/read", trigger.clone()).await)["trigger"]["ingestPath"].is_string());
            }
            for caller in [bob, viewer] {
                let view = success(rpc(&endpoint, caller, "bots/triggers/read", trigger.clone()).await);
                assert!(view["trigger"]["ingestPath"].is_null());
                assert!(view["trigger"]["pairingCode"].is_null());
                let list = success(rpc(&endpoint, caller, "bots/triggers/list", json!({"botId":bot})).await);
                assert!(list["triggers"][0]["ingestPath"].is_null());
            }
            rejected(rpc(&endpoint, bob, "bots/triggers/delete", trigger).await);
            // Direct callers receive the same enforcement, including fresh revocation.
            let unscoped_api = api.clone();
            assert_eq!(tokio::spawn(async move { unscoped_api.list_profiles(api::ProfileListParams {}).await }).await?.unwrap_err().kind, AgentApiErrorKind::Rejected);
            let headers = { let mut h = axum::http::HeaderMap::new(); h.insert("authorization", format!("Bearer {}", viewer.secret.expose()).parse()?); h };
            let context = temporal_server::gateway::authentication::authenticate(&keys, &access, &headers,
                api::MethodAccess::Universe(UniverseAction::Read), 10).await?;
            let denied = with_request_context(context.clone(), api.rename_session(api::SessionRenameParams {
                session_id: session.into(), display_name: None })).await.unwrap_err();
            assert_eq!(denied.kind, AgentApiErrorKind::Rejected);
            access.apply(admin.id, AccessChange::RevokeRole { assignment: RoleAssignment {
                scope, subject: Subject::Principal(viewer.record.principal_id), role: Role::Viewer } }, 11).await?;
            assert_eq!(with_request_context(context, api.list_profiles(api::ProfileListParams {})).await.unwrap_err().kind, AgentApiErrorKind::Rejected);
            rejected(rpc(&endpoint, viewer, "profiles/list", json!({})).await);
            // A real run is accepted and completes through the worker's fake model.
            let run = success(rpc(&endpoint, alice, "session/runs/start", json!({"sessionId":session,"source":{"type":"input","items":[{"type":"text","text":"hello"}]}})).await);
            assert!(run["run"]["id"].is_string(), "{run}");
            // Elevated roles can stop, but still cannot delete someone else's personal session.
            rejected(rpc(&endpoint, bob, "session/close", json!({"sessionId":session,"force":true})).await);
            success(rpc(&endpoint, operator, "session/close", json!({"sessionId":session,"force":true})).await);
            rejected(rpc(&endpoint, universe_admin, "session/delete", json!({"sessionId":session})).await);
            success(rpc(&endpoint, alice, "session/delete", json!({"sessionId":session})).await);
            // Deleting content does not erase attribution or allow identity takeover.
            rejected(rpc(&endpoint, bob, "session/start", start).await);
            assert!(access.ownership(universe, &ResourceRef::Session(session.into())).await?.is_some());
            let admissions: i64 = sqlx::query_scalar("SELECT count(*) FROM access_action_audit WHERE universe_id=$1 AND actor->>'id'=$2 AND action='session/runs/start' AND admitted")
                .bind(universe).bind(alice.record.principal_id.to_string()).fetch_one(&pool).await?;
            assert!(admissions > 0);
            anyhow::Ok(())
        };
        let result = tokio::time::timeout(Duration::from_secs(150), outcome).await;
        server.abort();
        result?
    }).await
}
