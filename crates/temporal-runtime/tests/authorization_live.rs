//! Authenticated HTTP over real PostgreSQL and Temporal, with a fake model:
//! tokens and actors, unshared and shared sessions, list filters and sharing. Requires an explicitly selected disposable database.
mod support;

use base64::Engine as _;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};
use store_pg::PgStore;
use temporal_runtime::{
    DeploymentStores, UniverseRuntime,
    config::{GatewayAuthMode, RuntimeTokens},
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

async fn api_call(url: &str, caller: &Caller, method: &str, params: Value) -> Value {
    let universe = caller.universe.map(|id| id.to_string());
    let parts = api::http_request(method, universe.as_deref(), params).unwrap();
    let mut request = reqwest::Client::new()
        .request(
            reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).unwrap(),
            format!("{url}{}", parts.path),
        )
        .bearer_auth(&caller.secret);
    if let Some(body) = parts.body {
        request = request.json(&body);
    }
    if let Some(actor) = &caller.actor {
        request = request.header("x-lightspeed-actor", actor);
    }
    request.send().await.unwrap().json().await.unwrap()
}
#[track_caller]
fn success(value: Value) -> Value {
    assert!(value.get("kind").is_none(), "{value}");
    value
}
#[track_caller]
fn kind(value: &Value) -> &str {
    value["kind"].as_str().unwrap_or("success")
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
    let endpoint = format!("http://{}", listener.local_addr()?);
    let router = gateway_router(
        Arc::new(GatewayState::multi(
            GatewayAuthMode::Authenticated {
                tokens: Arc::new(RuntimeTokens::from_configuration(
                    [
                        (
                            "LIGHTSPEED_API_TOKEN_PLATFORM".into(),
                            PLATFORM_TOKEN.into(),
                        ),
                        (
                            "LIGHTSPEED_API_TOKEN_OPERATOR".into(),
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
            public: false,
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
            // Invalid credentials are rejected with a problem response before reading a body.
            for bearer in [None, Some(format!("lsk_{}", "0".repeat(48)))] {
                let mut request = reqwest::Client::new().get(format!("{endpoint}/api/v1/universes/{universe}/sessions/any"));
                if let Some(bearer) = &bearer {
                    request = request.bearer_auth(bearer);
                }
                let response = request.send().await?;
                assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED, "{bearer:?}");
                assert_eq!(response.headers()["www-authenticate"], "Bearer");
                let refusal: Value = response.json().await?;
                assert_eq!(kind(&refusal), "unauthenticated");
            }

            let alice = platform.as_actor("alice");
            let bob = platform.as_actor("bob");
            let suffix = Uuid::new_v4().simple().to_string();
            let draft = format!("draft-{suffix}");
            let team = format!("team-{suffix}");

            // A session started for an actor is unshared and names its
            // creator; a retry keeps both.
            let started = success(api_call(&endpoint, &alice, "session/start", json!({"sessionId": draft})).await);
            assert_eq!(started["session"]["id"], draft.as_str());
            let read = success(api_call(&endpoint, &alice, "session/read", json!({"sessionId": draft})).await);
            assert_eq!(read["session"]["access"]["visibility"], "restricted");
            assert_eq!(read["session"]["access"]["createdBy"], json!({"kind":"actor","id":"alice"}));
            success(api_call(&endpoint, &bob, "session/start", json!({"sessionId": draft, "access": {"visibility": "universe"}})).await);
            let read = success(api_call(&endpoint, &bob, "session/read", json!({"sessionId": draft})).await);
            assert_eq!(read["session"]["access"]["visibility"], "restricted");
            assert_eq!(read["session"]["access"]["createdBy"]["id"], "alice");

            // A session may be shared from the start; a key acting for
            // itself is recorded as that key.
            success(api_call(&endpoint, &bob, "session/start", json!({"sessionId": team, "access": {"visibility": "universe"}})).await);
            let own = success(api_call(&endpoint, &developer, "session/start", json!({})).await);
            let own = own["session"]["id"].as_str().unwrap().to_owned();
            let read = success(api_call(&endpoint, &developer, "session/read", json!({"sessionId": own})).await);
            assert_eq!(
                read["session"]["access"]["createdBy"],
                json!({"kind": "token", "name": "operator"})
            );

            // Lists filter by each session's root, in one pageable query. An
            // actor sees what it created and what is shared, and no one else's
            // unshared work.
            let visible_to_bob = listed(&success(api_call(&endpoint, &bob, "session/list", json!({"visibleTo": "bob", "limit": 200})).await));
            assert!(visible_to_bob.contains(&team));
            assert!(!visible_to_bob.contains(&draft));
            let visible_to_alice = listed(&success(api_call(&endpoint, &alice, "session/list", json!({"visibleTo": "alice", "limit": 200})).await));
            assert!(visible_to_alice.contains(&draft) && visible_to_alice.contains(&team));
            let everything = listed(&success(api_call(&endpoint, &alice, "session/list", json!({"limit": 200})).await));
            assert!(everything.contains(&draft) && everything.contains(&team) && everything.contains(&own));

            // Sharing is one way.
            let shared = success(api_call(&endpoint, &alice, "session/share", json!({"sessionId": draft})).await);
            assert_eq!(shared["access"]["visibility"], "universe");
            assert_eq!(kind(&api_call(&endpoint, &alice, "session/share", json!({"sessionId": draft})).await), "rejected");
            assert_eq!(kind(&api_call(&endpoint, &alice, "session/share", json!({"sessionId": "missing"})).await), "notFound");
            let visible_to_bob = listed(&success(api_call(&endpoint, &bob, "session/list", json!({"visibleTo": "bob", "limit": 200})).await));
            assert!(visible_to_bob.contains(&draft));

            // Actors never change a trusted component's access.
            success(api_call(&endpoint, &developer.as_actor("mallory"), "session/read", json!({"sessionId": team})).await);

            let bytes = b"shared attachment";
            let uploaded = success(api_call(&endpoint, &platform, "blobs/put", json!({"blobs":[{"bytesBase64": base64::engine::general_purpose::STANDARD.encode(bytes)}]})).await);
            let blob = uploaded["blobs"][0]["blobRef"].clone();
            let parts = api::http_request("blobs/read", Some(&universe.to_string()), json!({"blobRef":blob}))?;
            let read = reqwest::Client::new().get(format!("{endpoint}{}", parts.path)).bearer_auth(&developer.secret).send().await?;
            assert_eq!(read.status(), reqwest::StatusCode::OK);
            assert_eq!(read.bytes().await?.as_ref(), bytes);


            anyhow::Ok(())
        }
        .await;
        server.abort();
        outcome
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires disposable Postgres and Temporal; no external OAuth provider; serialized"]
async fn listeners_isolate_api_public_callbacks_and_environment_routes() -> anyhow::Result<()> {
    let _lock = support::live::LIVE_TEST_LOCK.lock().await;
    let (_pool, universe) = disposable_pool().await?;
    let activities = support::live::fake_worker_activities().await?;
    support::live::run_with_live_worker(activities, move |client, queue, _session_id| async move {
        let private_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let public_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let environment_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let private_url = format!("http://{}", private_listener.local_addr()?);
        let public_url = format!("http://{}", public_listener.local_addr()?);
        let environment_url = format!("http://{}", environment_listener.local_addr()?);
        let runtime = Arc::new(UniverseRuntime::new_with_environment_gateway(
            client, queue, Some(public_url.clone()), DeploymentStores::from_env().await?,
            Some(environment_url.clone()),
        )?);
        let state = Arc::new(GatewayState::multi(
            GatewayAuthMode::Authenticated {
                tokens: Arc::new(RuntimeTokens::from_configuration(
                    [("LIGHTSPEED_API_TOKEN_PLATFORM".into(), PLATFORM_TOKEN.into())],
                    |_| unreachable!(),
                )?),
            }, runtime, public_url.clone(),
        ));
        let token_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let token_url = format!("http://{}/token", token_listener.local_addr()?);
        type TokenForm = std::collections::HashMap<String, String>;
        type CapturedTokenForms = Arc<tokio::sync::Mutex<Vec<TokenForm>>>;
        let token_forms: CapturedTokenForms = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let token_router = axum::Router::new().route("/token", axum::routing::post(
            |axum::extract::State(forms): axum::extract::State<CapturedTokenForms>,
             axum::Form(form): axum::Form<TokenForm>| async move {
                forms.lock().await.push(form);
                axum::Json(json!({"access_token":"provider-test-access-token", "token_type":"Bearer", "expires_in":3600}))
            }
        )).with_state(token_forms.clone());
        let mut servers = vec![tokio::spawn(async move { axum::serve(token_listener, token_router).await })];
        for (listener, routes) in [
            (private_listener, GatewayRoutes::API),
            (public_listener, GatewayRoutes::PUBLIC),
            (environment_listener, GatewayRoutes::ENVIRONMENT),
        ] {
            let router = gateway_router(state.clone(), 1024 * 1024, routes);
            servers.push(tokio::spawn(async move { axum::serve(listener, router).await }));
        }
        let outcome = async {
            let http = reqwest::Client::new();
            for base in [&private_url, &public_url, &environment_url] {
                for path in ["/health", "/ready"] {
                    assert_eq!(http.get(format!("{base}{path}")).send().await?.status(), reqwest::StatusCode::OK);
                }
            }
            for base in [&private_url, &public_url] {
                for path in ["/environments/connect", "/environments/data", "/environments/routes/universe/environment"] {
                    assert_eq!(http.get(format!("{base}{path}")).send().await?.status(), reqwest::StatusCode::NOT_FOUND);
                }
            }
            for base in [&public_url, &environment_url] {
                assert_eq!(http.post(format!("{base}/rpc")).json(&json!({"id":1,"method":"initialize","params":{}}))
                    .send().await?.status(), reqwest::StatusCode::NOT_FOUND);
                assert_eq!(http.get(format!("{base}/api/v1/connection")).send().await?.status(), reqwest::StatusCode::NOT_FOUND);
            }
            assert_eq!(http.post(format!("{private_url}/rpc")).body("{}")
                .send().await?.status(), reqwest::StatusCode::NOT_FOUND);
            assert_eq!(http.get(format!("{private_url}/api/v1/connection")).send().await?.status(), reqwest::StatusCode::UNAUTHORIZED);
            for base in [&private_url, &environment_url] {
                for path in ["/ingress/credentials/callback", "/ingress/credentials/client-metadata.json"] {
                    assert_eq!(http.get(format!("{base}{path}")).send().await?.status(), reqwest::StatusCode::NOT_FOUND);
                }
                assert_eq!(http.post(format!("{base}/ingress/webhooks/bots/{universe}/bot/trigger/token")).body("{}").send().await?.status(), reqwest::StatusCode::NOT_FOUND);
            }
            let metadata = http.get(format!("{public_url}/ingress/credentials/client-metadata.json")).send().await?;
            assert_eq!(metadata.status(), reqwest::StatusCode::OK);
            let metadata: Value = metadata.json().await?;
            assert_eq!(metadata["client_id"], format!("{public_url}/ingress/credentials/client-metadata.json"));
            assert_eq!(metadata["redirect_uris"], json!([format!("{public_url}/ingress/credentials/callback")]));
            for old_path in ["/auth/callback", "/auth/client-metadata.json", "/credentials/callback", "/credentials/client-metadata.json"] {
                assert_eq!(http.get(format!("{public_url}{old_path}")).send().await?.status(), reqwest::StatusCode::NOT_FOUND);
            }
            assert_eq!(http.post(format!("{public_url}/hooks/bots/{universe}/bot/trigger/token")).body("{}").send().await?.status(), reqwest::StatusCode::NOT_FOUND);
            for old_path in ["/environment-gateway/connect", "/environment-gateway/data", "/environment-gateway/routes/universe/environment", "/environment-gateway/peer/streams", "/environment-gateway/peer/data"] {
                assert_eq!(http.get(format!("{environment_url}{old_path}")).send().await?.status(), reqwest::StatusCode::NOT_FOUND);
            }

            // Start through the authenticated private API. The browser callback
            // has no token or universe header and resolves the stored flow by
            // its one-time state. A denied login exercises that whole HTTP path
            // without contacting an external OAuth provider.
            let caller = Caller { secret: PLATFORM_TOKEN.into(), universe: Some(universe), actor: Some("alice".into()) };
            let endpoint = private_url.clone();
            let created = success(api_call(&endpoint, &caller, "credentials/clients/create", json!({
                "providerKind":"customOAuth", "authorizationEndpoint":"https://example.com/authorize",
                "tokenEndpoint":token_url, "remoteClientId":"browser-client", "tokenEndpointAuthMethod":"none"
            })).await);
            let started = success(api_call(&endpoint, &caller, "credentials/flows/start", json!({"clientId":created["client"]["clientId"]})).await);
            let authorize_url = url::Url::parse(started["authorizeUrl"].as_str().expect("authorization URL"))?;
            let state = authorize_url.query_pairs().find(|(key, _)| key == "state").expect("state").1.into_owned();
            assert_eq!(authorize_url.query_pairs().find(|(key, _)| key == "redirect_uri").expect("redirect URI").1, format!("{public_url}/ingress/credentials/callback"));
            let callback = format!("{public_url}/ingress/credentials/callback");
            let response = http.get(&callback).query(&[("state", state.as_str()), ("error", "access_denied")]).send().await?;
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            assert!(response.text().await?.contains("Authorization failed"));
            let read = success(api_call(&endpoint, &caller, "credentials/flows/read", json!({"flowId":started["flowId"]})).await);
            assert_eq!(read["flow"]["status"], "failed");
            assert_eq!(http.get(&callback).query(&[("state", state.as_str()), ("error", "access_denied")]).send().await?.status(), reqwest::StatusCode::BAD_REQUEST);
            assert!(token_forms.lock().await.is_empty(), "denial never exchanges a token");

            let started = success(api_call(&endpoint, &caller, "credentials/flows/start", json!({"clientId":created["client"]["clientId"]})).await);
            let authorize_url = url::Url::parse(started["authorizeUrl"].as_str().expect("authorization URL"))?;
            let state = authorize_url.query_pairs().find(|(key, _)| key == "state").expect("state").1.into_owned();
            let challenge = authorize_url.query_pairs().find(|(key, _)| key == "code_challenge").expect("PKCE challenge").1.into_owned();
            let response = http.get(&callback).query(&[("state", state.as_str()), ("code", "browser-code")]).send().await?;
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            let page = response.text().await?;
            assert!(page.contains("Authorization complete"), "{page}");
            assert!(!page.contains("provider-test-access-token"));
            let read = success(api_call(&endpoint, &caller, "credentials/flows/read", json!({"flowId":started["flowId"]})).await);
            assert_eq!(read["flow"]["status"], "completed");
            let grant = success(api_call(&endpoint, &caller, "credentials/grants/read", json!({"grantId":read["flow"]["grantId"]})).await);
            assert_eq!(grant["grant"]["hasAccessToken"], true);
            assert_eq!(grant["grant"]["createdBy"], json!({"kind":"actor", "id":"alice"}));
            assert_eq!(grant["grant"]["status"], "active");
            assert_eq!(http.get(&callback).query(&[("state", state.as_str()), ("code", "browser-code")]).send().await?.status(), reqwest::StatusCode::BAD_REQUEST);
            let forms = token_forms.lock().await;
            assert_eq!(forms.len(), 1, "replay never exchanges the code again");
            assert_eq!(forms[0]["grant_type"], "authorization_code");
            assert_eq!(forms[0]["code"], "browser-code");
            assert_eq!(forms[0]["redirect_uri"], format!("{public_url}/ingress/credentials/callback"));
            assert_eq!(credentials::pkce_challenge_s256(&credentials::SecretValue::new(&forms[0]["code_verifier"])), challenge);
            Ok::<_, anyhow::Error>(())
        }.await;
        for server in servers { server.abort(); }
        outcome
    }).await
}

async fn resource_json(
    response: reqwest::Response,
    expected: reqwest::StatusCode,
) -> anyhow::Result<Value> {
    let status = response.status();
    let content_type = response.headers()["content-type"].to_str()?.to_owned();
    let body: Value = response.json().await?;
    assert_eq!(status, expected, "{body}");
    assert_eq!(content_type, "application/json");
    assert!(body.get("result").is_none(), "direct response DTO: {body}");
    Ok(body)
}

async fn resource_problem(
    response: reqwest::Response,
    expected: reqwest::StatusCode,
    expected_kind: api::AgentApiErrorKind,
) -> anyhow::Result<()> {
    let status = response.status();
    assert_eq!(
        response.headers()["content-type"],
        "application/problem+json"
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
    let problem: api::HttpProblem = response.json().await?;
    assert_eq!(status, expected, "{problem:?}");
    assert_eq!(problem.status, expected.as_u16());
    assert_eq!(problem.kind, expected_kind);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires disposable Postgres and Temporal; no model calls; serialized"]
async fn resource_http_profiles_binary_ownership_and_watch() -> anyhow::Result<()> {
    use api::AgentApiErrorKind::{InvalidRequest, NotFound, Unauthenticated};
    use reqwest::StatusCode;
    use std::time::Duration;

    let _lock = support::live::LIVE_TEST_LOCK.lock().await;
    let (_pool, universe) = disposable_pool().await?;
    let activities = support::live::fake_worker_activities().await?;
    support::live::run_with_live_worker(activities, move |client, queue, _session_id| async move {
        let runtime = Arc::new(UniverseRuntime::new(
            client, queue, None, DeploymentStores::from_env().await?,
        )?);
        let (endpoint, server) = serve(runtime).await?;
        let base = &endpoint;
        let outcome = async {
            let http = reqwest::Client::new();
            let connection_url = format!("{base}/api/v1/connection");
            resource_problem(http.get(&connection_url).send().await?, StatusCode::UNAUTHORIZED, Unauthenticated).await?;
            let connection = resource_json(http.get(&connection_url).bearer_auth(PLATFORM_TOKEN).send().await?, StatusCode::OK).await?;
            assert_eq!(connection["runtime"], "runtime");
            assert_eq!(connection["contractRevision"], api::HTTP_CONTRACT_REVISION);
            assert_eq!(connection["caller"], json!({"single":false,"tokenName":"platform"}));
            assert!(connection["serverInfo"]["name"].is_string());
            let operator = resource_json(http.get(&connection_url).bearer_auth(OPERATOR_TOKEN).send().await?, StatusCode::OK).await?;
            assert_eq!(operator["caller"]["tokenName"], "operator");
            resource_problem(http.get(&connection_url).bearer_auth(PLATFORM_TOKEN)
                .header("x-lightspeed-universe", universe.to_string()).send().await?, StatusCode::BAD_REQUEST, InvalidRequest).await?;

            let other_universe = Uuid::new_v4();
            let other_handle = format!("http-{}", Uuid::new_v4().simple());
            let created = resource_json(http.post(format!("{base}/api/v1/universes")).bearer_auth(PLATFORM_TOKEN)
                .json(&json!({"universeId":other_universe,"handle":other_handle})).send().await?, StatusCode::CREATED).await?;
            assert_eq!(created["created"], true);
            let retried = resource_json(http.post(format!("{base}/api/v1/universes")).bearer_auth(PLATFORM_TOKEN)
                .json(&json!({"universeId":other_universe,"handle":other_handle})).send().await?, StatusCode::OK).await?;
            assert_eq!(retried["created"], false);
            let other_profiles = resource_json(http.get(format!("{base}/api/v1/universes/{other_handle}/profiles"))
                .bearer_auth(PLATFORM_TOKEN).send().await?, StatusCode::OK).await?;
            assert_eq!(other_profiles["profiles"], json!([]));

            let universe_url = format!("{base}/api/v1/universes/{universe}");
            let profile_id = format!("http-{}", Uuid::new_v4().simple());
            let profile_url = format!("{universe_url}/profiles/{profile_id}");
            let created = resource_json(http.put(&profile_url).bearer_auth(PLATFORM_TOKEN)
                .json(&json!({"profile":{"displayName":"HTTP profile","metadata":{"owner":"test"}}}))
                .send().await?, StatusCode::OK).await?;
            assert_eq!(created["profile"]["profileId"], profile_id);
            assert_eq!(created["profile"]["metadata"], json!({"owner":"test"}));
            let revision = created["profile"]["revision"].clone();
            let updated = resource_json(http.put(&profile_url).bearer_auth(PLATFORM_TOKEN)
                .json(&json!({"profile":{"displayName":"Updated"},"expectedRevision":revision}))
                .send().await?, StatusCode::OK).await?;
            assert_eq!(updated["profile"]["displayName"], "Updated");
            let read = resource_json(http.get(&profile_url).bearer_auth(PLATFORM_TOKEN).send().await?, StatusCode::OK).await?;
            assert_eq!(read["profile"], updated["profile"]);
            let listed = resource_json(http.get(format!("{universe_url}/profiles")).bearer_auth(PLATFORM_TOKEN).send().await?, StatusCode::OK).await?;
            assert!(listed["profiles"].as_array().expect("profile list").iter().any(|profile| profile["profileId"] == profile_id));

            for body in [
                json!({"profile":{"profileId":profile_id}}),
                json!({"profile":{"profileId":"forged"}}),
                json!({"profile":{},"surprise":true}),
                json!({"profile":{"surprise":true}}),
            ] {
                resource_problem(http.put(&profile_url).bearer_auth(PLATFORM_TOKEN).json(&body).send().await?, StatusCode::BAD_REQUEST, InvalidRequest).await?;
            }
            resource_problem(http.get(&profile_url).bearer_auth(PLATFORM_TOKEN).query(&[("unknown","value")]).send().await?, StatusCode::BAD_REQUEST, InvalidRequest).await?;
            resource_problem(http.get(&profile_url).bearer_auth(PLATFORM_TOKEN).body("{}").send().await?, StatusCode::BAD_REQUEST, InvalidRequest).await?;
            resource_problem(http.put(&profile_url).bearer_auth(PLATFORM_TOKEN).header("content-type","application/json")
                .body(r#"{"profile":{},"profile":{}}"#).send().await?, StatusCode::BAD_REQUEST, InvalidRequest).await?;
            resource_problem(http.get(format!("{universe_url}/sessions?limit=1&limit=2")).bearer_auth(PLATFORM_TOKEN)
                .send().await?, StatusCode::BAD_REQUEST, InvalidRequest).await?;
            resource_problem(http.get(format!("{base}/api/v1/no-such-resource")).bearer_auth(PLATFORM_TOKEN)
                .send().await?, StatusCode::NOT_FOUND, NotFound).await?;

            // A payload larger than a transfer chunk exercises actual storage range reads.
            let mut bytes: Vec<u8> = (0..700_000).map(|index| (index % 251) as u8).collect();
            bytes.extend_from_slice(Uuid::new_v4().as_bytes());
            let blob_ref = harness::BlobRef::from_bytes(&bytes);
            let blob_url = format!("{universe_url}/blobs/{blob_ref}/content");
            let uploaded = resource_json(http.put(&blob_url).bearer_auth(PLATFORM_TOKEN)
                .header("content-type","application/octet-stream").body(bytes.clone()).send().await?, StatusCode::OK).await?;
            assert_eq!(uploaded["blobRef"], blob_ref.as_str());
            assert_eq!(uploaded["bytes"], bytes.len() as u64);
            let downloaded = http.get(&blob_url).bearer_auth(PLATFORM_TOKEN).send().await?;
            assert_eq!(downloaded.status(), StatusCode::OK);
            assert_eq!(downloaded.headers()["content-type"], "application/octet-stream");
            assert_eq!(downloaded.content_length(), Some(bytes.len() as u64));
            assert_eq!(downloaded.bytes().await?.as_ref(), bytes.as_slice());
            resource_problem(http.get(format!("{base}/api/v1/universes/{other_handle}/blobs/{blob_ref}/content"))
                .bearer_auth(PLATFORM_TOKEN).send().await?, StatusCode::NOT_FOUND, NotFound).await?;

            let wrong_ref = harness::BlobRef::from_bytes(Uuid::new_v4().as_bytes());
            let wrong_url = format!("{universe_url}/blobs/{wrong_ref}/content");
            resource_problem(http.put(&wrong_url).bearer_auth(PLATFORM_TOKEN).header("content-type","application/octet-stream")
                .body(bytes).send().await?, StatusCode::BAD_REQUEST, InvalidRequest).await?;
            resource_problem(http.get(&wrong_url).bearer_auth(PLATFORM_TOKEN).send().await?, StatusCode::NOT_FOUND, NotFound).await?;

            let events_url = format!("{universe_url}/events?kinds=session");
            assert_eq!(http.head(&events_url).send().await?.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(http.head(&events_url).bearer_auth(PLATFORM_TOKEN).send().await?.status(), StatusCode::METHOD_NOT_ALLOWED);
            let mut events = http.get(&events_url).bearer_auth(PLATFORM_TOKEN).header("accept","text/event-stream").send().await?;
            assert_eq!(events.status(), StatusCode::OK);
            assert_eq!(events.headers()["content-type"], "text/event-stream");
            assert_eq!(events.headers()["cache-control"], "no-store");
            let frame = tokio::time::timeout(Duration::from_secs(5), async {
                let mut frame = Vec::new();
                loop {
                    let chunk = events.chunk().await?.expect("watch reset before end of stream");
                    frame.extend_from_slice(&chunk);
                    if frame.windows(2).any(|pair| pair == b"\n\n") {
                        return anyhow::Ok(String::from_utf8(frame)?);
                    }
                }
            }).await??;
            let frame = frame.split("\n\n").next().expect("first SSE frame");
            assert!(frame.lines().any(|line| line.strip_prefix("event:").is_some_and(|event| event.trim() == "watch/reset")), "{frame}");
            let data = frame.lines().find_map(|line| line.strip_prefix("data:")).expect("reset payload");
            assert_eq!(serde_json::from_str::<Value>(data.trim())?, json!({}));
            drop(events);

            resource_json(http.delete(&profile_url).bearer_auth(PLATFORM_TOKEN).send().await?, StatusCode::OK).await?;
            resource_problem(http.get(&profile_url).bearer_auth(PLATFORM_TOKEN).send().await?, StatusCode::NOT_FOUND, NotFound).await?;
            anyhow::Ok(())
        }.await;
        server.abort();
        outcome
    }).await
}
