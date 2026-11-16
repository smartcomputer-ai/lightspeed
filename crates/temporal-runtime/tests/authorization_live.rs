//! Authenticated HTTP over real PostgreSQL and Temporal, with a fake model:
//! tokens and actors, unshared and shared sessions, list filters and sharing. Requires an explicitly selected disposable database.
mod support;

use base64::Engine as _;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};
use store_pg::PgStore;
use temporal_runtime::{
    DeploymentStores, UniverseRuntime,
    config::{GatewayAuthMode, RpcTokens},
    gateway::{GatewayRoutes, GatewayState, gateway_router},
};
use uuid::Uuid;

/// A configured component token, explicit universe, and optional actor.
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

const PLATFORM_TOKEN: &str = "platform-secret-with-at-least-32-characters";
const OPERATOR_TOKEN: &str = "operator-secret-with-at-least-32-characters";

/// The authenticated gateway over `runtime`, served on a free local port.
async fn serve(
    runtime: Arc<UniverseRuntime>,
) -> anyhow::Result<(String, tokio::task::JoinHandle<std::io::Result<()>>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/rpc", listener.local_addr()?);
    let router = gateway_router(
        Arc::new(GatewayState::multi(
            GatewayAuthMode::Authenticated {
                tokens: Arc::new(RpcTokens::from_configuration(
                    [
                        (
                            "LIGHTSPEED_RPC_TOKEN_PLATFORM".into(),
                            PLATFORM_TOKEN.into(),
                        ),
                        (
                            "LIGHTSPEED_RPC_TOKEN_OPERATOR".into(),
                            OPERATOR_TOKEN.into(),
                        ),
                    ],
                    |_| unreachable!(),
                )?),
            },
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
    // `seed_agent_default` also creates the universe. The sessions here omit
    // the model, and a disposable database has no default until one is set.
    support::live::seed_agent_default(
        &PgStore::new(pool.clone(), store_pg::PgStoreConfig::new(universe)),
        &support::live::openai_live_model(),
    )
    .await?;
    Ok((pool, universe))
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires disposable Postgres and Temporal; fake model; serialized"]
async fn tokens_actors_unshared_work_and_sharing() -> anyhow::Result<()> {
    let _lock = support::live::LIVE_TEST_LOCK.lock().await;
    let (_pool, universe) = disposable_pool().await?;
    let platform = Caller {
        secret: PLATFORM_TOKEN.into(),
        universe: Some(universe),
        actor: None,
    };
    let developer = Caller {
        secret: OPERATOR_TOKEN.into(),
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
            // A request that presents no valid key is answered 401 from its
            // headers alone. Its body was not read, so the JSON-RPC error
            // answers no request id.
            let read = json!({"id":1,"method":"session/read","params":{"sessionId":"any"}});
            for bearer in [None, Some(format!("lsk_{}", "0".repeat(48)))] {
                let mut request = reqwest::Client::new().post(&endpoint).json(&read);
                if let Some(bearer) = &bearer {
                    request = request.bearer_auth(bearer);
                }
                let response = request.send().await?;
                assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED, "{bearer:?}");
                assert_eq!(response.headers()["www-authenticate"], "Bearer");
                let refusal: Value = response.json().await?;
                assert!(refusal["id"].is_null(), "{refusal}");
                assert_eq!(kind(&refusal), "unauthenticated");
            }

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
            assert_eq!(
                read["session"]["access"]["createdBy"],
                json!({"kind": "token", "name": "operator"})
            );

            // Lists filter by each session's root, in one pageable query. An
            // actor sees what it created and what is shared, and no one else's
            // unshared work.
            let visible_to_bob = listed(&success(rpc(&endpoint, &bob, "session/list", json!({"visibleTo": "bob", "limit": 200})).await));
            assert!(visible_to_bob.contains(&team));
            assert!(!visible_to_bob.contains(&draft));
            let visible_to_alice = listed(&success(rpc(&endpoint, &alice, "session/list", json!({"visibleTo": "alice", "limit": 200})).await));
            assert!(visible_to_alice.contains(&draft) && visible_to_alice.contains(&team));
            let everything = listed(&success(rpc(&endpoint, &alice, "session/list", json!({"limit": 200})).await));
            assert!(everything.contains(&draft) && everything.contains(&team) && everything.contains(&own));

            // Sharing is one way.
            let shared = success(rpc(&endpoint, &alice, "session/share", json!({"sessionId": draft})).await);
            assert_eq!(shared["access"]["visibility"], "universe");
            assert_eq!(kind(&rpc(&endpoint, &alice, "session/share", json!({"sessionId": draft})).await), "rejected");
            assert_eq!(kind(&rpc(&endpoint, &alice, "session/share", json!({"sessionId": "missing"})).await), "notFound");
            let visible_to_bob = listed(&success(rpc(&endpoint, &bob, "session/list", json!({"visibleTo": "bob", "limit": 200})).await));
            assert!(visible_to_bob.contains(&draft));

            // Actors never change a trusted component's access.
            success(rpc(&endpoint, &developer.as_actor("mallory"), "session/read", json!({"sessionId": team})).await);

            let bytes = b"shared attachment";
            let uploaded = success(rpc(&endpoint, &platform, "blobs/put", json!({"blobs":[{"bytesBase64": base64::engine::general_purpose::STANDARD.encode(bytes)}]})).await);
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
