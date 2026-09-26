//! Authenticated HTTP over real PostgreSQL and Temporal, with a fake model:
//! keys and actors, unshared and shared sessions, list filters and sharing. Requires an explicitly selected disposable database.
mod support;

use api::{AccessScope, Attribution, MethodGroup};
use auth::{ApiKeySpec, MintedApiKey};
use base64::Engine as _;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};
use store_pg::{PgApiKeyStore, PgStore};
use temporal_server::{
    DeploymentStores, UniverseRuntime,
    config::GatewayAuthMode,
    gateway::{GatewayRoutes, GatewayState, gateway_router},
};
use uuid::Uuid;

/// One way of calling the gateway: a key, the universe it names when it is a
/// deployment key, and the actor it asserts.
#[derive(Clone)]
struct Caller {
    secret: String,
    universe: Option<Uuid>,
    actor: Option<String>,
}

impl Caller {
    fn as_actor(&self, actor: &str) -> Self {
        Self {
            actor: Some(actor.to_owned()),
            ..self.clone()
        }
    }
}

async fn rpc(url: &str, caller: &Caller, method: &str, params: Value) -> Value {
    let mut request = reqwest::Client::new()
        .post(url)
        .bearer_auth(&caller.secret)
        .json(&json!({"id":1,"method":method,"params":params}));
    if let Some(universe) = caller.universe {
        request = request.header("x-lightspeed-universe", universe.to_string());
    }
    if let Some(actor) = &caller.actor {
        request = request.header("x-lightspeed-actor", actor);
    }
    request.send().await.unwrap().json().await.unwrap()
}
#[track_caller]
fn success(value: Value) -> Value {
    assert!(value.get("error").is_none(), "{value}");
    value["result"]["result"].clone()
}
#[track_caller]
fn kind(value: &Value) -> &str {
    value["error"]["data"]["kind"].as_str().unwrap_or("success")
}
fn listed(value: &Value) -> BTreeSet<String> {
    value["sessions"]
        .as_array()
        .expect("session list")
        .iter()
        .map(|session| session["id"].as_str().unwrap().to_owned())
        .collect()
}

async fn issue(
    keys: &PgApiKeyStore,
    scope: AccessScope,
    groups: Option<&[MethodGroup]>,
    assert_actor: bool,
) -> anyhow::Result<MintedApiKey> {
    let key = auth::mint_api_key(
        ApiKeySpec {
            scope,
            groups: groups.map(|groups| groups.iter().copied().collect()),
            assert_actor,
            created_by: Attribution::Local,
            display_name: Some("authorization live".into()),
        },
        4,
    )?;
    keys.create_api_key(&key.key_hash, &key.record).await?;
    Ok(key)
}

/// The authenticated gateway over `runtime`, served on a free local port.
async fn serve(
    runtime: Arc<UniverseRuntime>,
) -> anyhow::Result<(String, tokio::task::JoinHandle<std::io::Result<()>>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/rpc", listener.local_addr()?);
    let router = gateway_router(
        Arc::new(GatewayState::multi(
            GatewayAuthMode::Authenticated,
            runtime,
            endpoint.clone(),
        )),
        1024 * 1024,
        GatewayRoutes {
            api: true,
            environment: false,
        },
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    Ok((endpoint, server))
}

async fn disposable_pool() -> anyhow::Result<(sqlx::PgPool, Uuid)> {
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
    Ok((pool, universe))
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires disposable Postgres and Temporal; fake model; serialized"]
async fn keys_actors_unshared_work_and_sharing() -> anyhow::Result<()> {
    let _lock = support::live::LIVE_TEST_LOCK.lock().await;
    let (pool, universe) = disposable_pool().await?;
    let keys = PgApiKeyStore::new(pool.clone());
    let gate = issue(&keys, AccessScope::Deployment, None, true).await?;
    let universe_key = issue(
        &keys,
        AccessScope::Universe {
            universe_id: universe,
        },
        None,
        false,
    )
    .await?;
    let connector = issue(
        &keys,
        AccessScope::Deployment,
        Some(&[MethodGroup::ChannelsInbound, MethodGroup::BlobsPut]),
        false,
    )
    .await?;
    let platform = Caller {
        secret: gate.secret.expose().to_owned(),
        universe: Some(universe),
        actor: None,
    };
    let developer = Caller {
        secret: universe_key.secret.expose().to_owned(),
        universe: None,
        actor: None,
    };
    let connector = Caller {
        secret: connector.secret.expose().to_owned(),
        universe: Some(universe),
        actor: None,
    };
    let activities = support::live::fake_worker_activities().await?;
    support::live::run_with_live_worker(activities, move |client, queue, _session_id| async move {
        let runtime = Arc::new(UniverseRuntime::new(
            client,
            queue,
            None,
            DeploymentStores::from_env().await?,
        )?);
        let (endpoint, server) = serve(runtime).await?;
        let outcome = async {
            let alice = platform.as_actor("alice");
            let bob = platform.as_actor("bob");
            let suffix = Uuid::new_v4().simple().to_string();
            let draft = format!("draft-{suffix}");
            let team = format!("team-{suffix}");

            // A session started for an actor is unshared and names its
            // creator; a retry keeps both.
            let started = success(rpc(&endpoint, &alice, "session/start", json!({"sessionId": draft})).await);
            assert_eq!(started["session"]["id"], draft.as_str());
            let read = success(rpc(&endpoint, &alice, "session/read", json!({"sessionId": draft})).await);
            assert_eq!(read["session"]["access"]["visibility"], "restricted");
            assert_eq!(read["session"]["access"]["createdBy"], json!({"kind":"actor","id":"alice"}));
            success(rpc(&endpoint, &bob, "session/start", json!({"sessionId": draft, "access": {"visibility": "universe"}})).await);
            let read = success(rpc(&endpoint, &bob, "session/read", json!({"sessionId": draft})).await);
            assert_eq!(read["session"]["access"]["visibility"], "restricted");
            assert_eq!(read["session"]["access"]["createdBy"]["id"], "alice");

            // A session may be shared from the start; a key acting for
            // itself is recorded as that key.
            success(rpc(&endpoint, &bob, "session/start", json!({"sessionId": team, "access": {"visibility": "universe"}})).await);
            let own = success(rpc(&endpoint, &developer, "session/start", json!({})).await);
            let own = own["session"]["id"].as_str().unwrap().to_owned();
            let read = success(rpc(&endpoint, &developer, "session/read", json!({"sessionId": own})).await);
            assert_eq!(read["session"]["access"]["createdBy"]["kind"], "key");
            assert_eq!(read["session"]["access"]["createdBy"]["prefix"], universe_key.record.key_prefix.as_str());

            // Lists filter by each session's root, in one pageable query.
            let visible_to_bob = listed(&success(rpc(&endpoint, &bob, "session/list", json!({"visibleTo": "bob", "limit": 200})).await));
            assert!(visible_to_bob.contains(&team));
            assert!(!visible_to_bob.contains(&draft));
            let alices = listed(&success(rpc(&endpoint, &alice, "session/list", json!({"createdBy": "alice", "limit": 200})).await));
            assert!(alices.contains(&draft) && !alices.contains(&team));
            let everything = listed(&success(rpc(&endpoint, &alice, "session/list", json!({"limit": 200})).await));
            assert!(everything.contains(&draft) && everything.contains(&team) && everything.contains(&own));

            // Sharing is one way.
            let shared = success(rpc(&endpoint, &alice, "session/share", json!({"sessionId": draft})).await);
            assert_eq!(shared["access"]["visibility"], "universe");
            assert_eq!(kind(&rpc(&endpoint, &alice, "session/share", json!({"sessionId": draft})).await), "rejected");
            assert_eq!(kind(&rpc(&endpoint, &alice, "session/share", json!({"sessionId": "missing"})).await), "not_found");
            let visible_to_bob = listed(&success(rpc(&endpoint, &bob, "session/list", json!({"visibleTo": "bob", "limit": 200})).await));
            assert!(visible_to_bob.contains(&draft));

            // A key asserts an actor only when allowed to, and calls only its
            // groups.
            assert_eq!(kind(&rpc(&endpoint, &developer.as_actor("mallory"), "session/read", json!({"sessionId": team})).await), "forbidden");
            assert_eq!(kind(&rpc(&endpoint, &connector, "session/read", json!({"sessionId": team})).await), "forbidden");
            assert_eq!(kind(&rpc(&endpoint, &connector, "session/list", json!({})).await), "forbidden");

            // A digest is a capability within the universe: bytes a connector
            // uploaded are readable by any key that may read blobs.
            let bytes = b"shared attachment";
            let uploaded = success(rpc(&endpoint, &connector, "blobs/put", json!({"blobs":[{"bytesBase64": base64::engine::general_purpose::STANDARD.encode(bytes)}]})).await);
            let blob = uploaded["blobs"][0]["blobRef"].clone();
            let read = success(rpc(&endpoint, &developer, "blobs/read", json!({"blobRef": blob})).await);
            assert_eq!(base64::engine::general_purpose::STANDARD.decode(read["bytesBase64"].as_str().unwrap())?, bytes);

            anyhow::Ok(())
        }
        .await;
        server.abort();
        outcome
    })
    .await
}
