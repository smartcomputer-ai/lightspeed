use std::{collections::HashMap, env, sync::Arc};

use auth::{
    AuthBrokerError, AuthCallback, AuthFlowStatus, AuthGrantExposure, AuthGrantStatus,
    AuthGrantStore, AuthRegistryError, AuthTokenBroker, HttpOAuthMetadataClient,
    HttpOAuthTokenClient, McpOAuthDriver, McpOAuthTarget, OAuthFlowService, OAuthRefreshRuntime,
    PrincipalRef, RegistryTokenBroker, SecretStore, StartAuthFlow, TokenAudience,
    parse_mcp_oauth_challenge,
};
use axum::{
    Form, Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header::WWW_AUTHENTICATE},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use store_pg::{PgStore, PgStoreConfig, SecretsMasterKey};
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;

#[derive(Default)]
struct OAuthMcpFixtureState {
    issuer: String,
    resource: String,
    registration_documents: Vec<Value>,
    token_forms: Vec<HashMap<String, String>>,
    refresh_count: usize,
    reject_refresh: bool,
    require_additional_scope: bool,
    mcp_authorizations: Vec<String>,
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Postgres env"]
async fn mcp_oauth_live_round_trip_restart_refresh_scope_and_reauth() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let store = live_store().await?;
    let (fixture, endpoint, fixture_task) = start_oauth_mcp_fixture().await?;
    let redirect_uri = "http://127.0.0.1:18999/auth/callback";

    let oauth_http = Arc::new(HttpOAuthMetadataClient::with_private_networks(true));
    let driver = McpOAuthDriver::new(store.clone(), store.clone(), oauth_http.clone());
    let client = driver
        .ensure_client(
            &McpOAuthTarget {
                server_id: "oauth-live".to_owned(),
                server_url: endpoint.clone(),
                scopes_default: vec!["tools.read".to_owned(), "offline_access".to_owned()],
                protected_resource_metadata_url: None,
                authorization_server_hint: None,
            },
            redirect_uri,
            None,
        )
        .await?;
    let issuer = fixture.lock().await.issuer.clone();
    assert_eq!(
        client.authorization_server_issuer.as_deref(),
        Some(issuer.as_str())
    );
    assert!(client.authorization_response_iss_parameter_supported);
    assert_eq!(
        client.authorization_server_scopes_supported,
        ["tools.read", "tools.write", "offline_access"]
    );
    assert_eq!(client.remote_client_id, "lightspeed-live-client");
    assert!(client.client_secret.is_none());

    let registration_documents = fixture.lock().await.registration_documents.clone();
    assert_eq!(registration_documents.len(), 1);
    assert_eq!(registration_documents[0]["application_type"], "web");
    assert_eq!(
        registration_documents[0]["token_endpoint_auth_method"],
        "none"
    );

    let token_client = Arc::new(HttpOAuthTokenClient::new_with_mcp_http(oauth_http)?);

    // An issuer failure is terminal and consumes the one-time callback state.
    let wrong_issuer_service = flow_service(store.clone(), token_client.clone(), 1_000);
    let wrong_issuer = wrong_issuer_service
        .start_flow(start_request(&client.client_id, redirect_uri))
        .await?;
    let wrong_state = authorization_parameter(&wrong_issuer.authorize_url, "state")?;
    assert_authorization_request(&wrong_issuer.authorize_url, &endpoint)?;
    let failed = wrong_issuer_service
        .complete_callback(AuthCallback {
            state: wrong_state.clone(),
            code: Some(auth::SecretValue::new("valid-code")),
            issuer: Some("http://wrong-issuer.invalid".to_owned()),
            error: None,
            error_description: None,
        })
        .await?;
    assert_eq!(failed.status(1_001), AuthFlowStatus::Failed);
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("issuer"))
    );
    assert!(matches!(
        wrong_issuer_service
            .complete_callback(AuthCallback {
                state: wrong_state,
                code: Some(auth::SecretValue::new("valid-code")),
                issuer: Some(issuer.clone()),
                error: None,
                error_description: None,
            })
            .await,
        Err(AuthRegistryError::FlowAlreadyConsumed { .. })
    ));

    // Start on one service instance, then reconstruct and finish on another.
    let starter = flow_service(store.clone(), token_client.clone(), 2_000);
    let started = starter
        .start_flow(start_request(&client.client_id, redirect_uri))
        .await?;
    let state = authorization_parameter(&started.authorize_url, "state")?;
    let challenge = authorization_parameter(&started.authorize_url, "code_challenge")?;
    assert_eq!(
        authorization_parameter(&started.authorize_url, "resource")?,
        endpoint
    );
    drop(starter);

    let finisher = flow_service(store.clone(), token_client.clone(), 2_100);
    let completed = finisher
        .complete_callback(AuthCallback {
            state: state.clone(),
            code: Some(auth::SecretValue::new("valid-code")),
            issuer: Some(issuer.clone()),
            error: None,
            error_description: None,
        })
        .await?;
    assert_eq!(
        completed.status(2_100),
        AuthFlowStatus::Completed,
        "callback failed: {:?}",
        completed.error
    );
    let grant_id = completed.grant_id.clone().expect("completed grant id");

    let code_form = fixture
        .lock()
        .await
        .token_forms
        .first()
        .cloned()
        .expect("authorization-code token request");
    assert_eq!(code_form.get("resource"), Some(&endpoint));
    assert_eq!(
        code_form.get("client_id").map(String::as_str),
        Some("lightspeed-live-client")
    );
    let verifier = code_form.get("code_verifier").expect("PKCE verifier");
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        challenge
    );

    assert!(matches!(
        finisher
            .complete_callback(AuthCallback {
                state,
                code: Some(auth::SecretValue::new("valid-code")),
                issuer: Some(issuer.clone()),
                error: None,
                error_description: None,
            })
            .await,
        Err(AuthRegistryError::FlowAlreadyConsumed { .. })
    ));

    let grant = store.read_grant(&grant_id).await?;
    assert_eq!(grant.audience.as_deref(), Some(endpoint.as_str()));
    assert_eq!(grant.status, AuthGrantStatus::Active);
    let initial_refresh_secret = grant.refresh_token_secret.clone().expect("refresh token");

    assert!(matches!(
        broker(store.clone(), token_client.clone(), 5_000)
            .bearer_token(
                &grant_id,
                &TokenAudience::McpResource(format!("{issuer}/other"))
            )
            .await,
        Err(AuthBrokerError::AudienceMismatch { .. })
    ));

    // Two concurrent readers share the database advisory lock and perform one
    // refresh; the returned refresh token rotates atomically.
    let active_broker = broker(store.clone(), token_client.clone(), 5_000);
    let left_audience = TokenAudience::McpResource(endpoint.clone());
    let right_audience = TokenAudience::McpResource(endpoint.clone());
    let (left, right) = tokio::join!(
        active_broker.bearer_token(&grant_id, &left_audience),
        active_broker.bearer_token(&grant_id, &right_audience),
    );
    assert_eq!(left?.expose(), "access-2");
    assert_eq!(right?.expose(), "access-2");
    assert_eq!(fixture.lock().await.refresh_count, 1);

    let refreshed = store.read_grant(&grant_id).await?;
    let rotated_refresh_secret = refreshed
        .refresh_token_secret
        .expect("rotated refresh token");
    assert_ne!(rotated_refresh_secret, initial_refresh_secret);
    assert_eq!(
        store.read_secret(&rotated_refresh_secret).await?.1.expose(),
        "refresh-2"
    );

    let tools = discover_tools(&endpoint, "access-2").await?;
    assert_eq!(tools, ["lookup"]);
    assert_eq!(
        fixture
            .lock()
            .await
            .mcp_authorizations
            .last()
            .map(String::as_str),
        Some("Bearer access-2")
    );

    fixture.lock().await.require_additional_scope = true;
    let challenged = reqwest::Client::new()
        .post(&endpoint)
        .bearer_auth("access-2")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
        .send()
        .await?;
    assert_eq!(challenged.status(), StatusCode::UNAUTHORIZED);
    let challenge = challenged
        .headers()
        .get(WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .expect("OAuth challenge");
    let parsed = parse_mcp_oauth_challenge(challenge, &endpoint)?;
    assert!(parsed.insufficient_scope);
    assert_eq!(parsed.required_scopes, ["tools.read", "tools.write"]);

    fixture.lock().await.reject_refresh = true;
    let rejected = broker(store.clone(), token_client, 10_000)
        .bearer_token(&grant_id, &TokenAudience::McpResource(endpoint.clone()))
        .await;
    assert!(matches!(
        rejected,
        Err(AuthBrokerError::GrantNotActive {
            status: AuthGrantStatus::NeedsReauth,
            ..
        })
    ));
    assert_eq!(
        store.read_grant(&grant_id).await?.status,
        AuthGrantStatus::NeedsReauth
    );

    fixture_task.abort();
    Ok(())
}

fn start_request(client_id: &auth::OAuthClientId, redirect_uri: &str) -> StartAuthFlow {
    StartAuthFlow {
        client_id: client_id.clone(),
        redirect_uri: redirect_uri.to_owned(),
        scopes: None,
        audience: None,
        grant_exposure: AuthGrantExposure::Brokered,
        principal: PrincipalRef {
            kind: auth::PrincipalKind::ServiceAccount,
            id: Some("test-service".into()),
        },
    }
}

fn flow_service(
    store: Arc<PgStore>,
    token_client: Arc<HttpOAuthTokenClient>,
    now_ms: i64,
) -> OAuthFlowService {
    OAuthFlowService::new(
        store.clone(),
        store.clone(),
        store.clone(),
        store,
        token_client,
    )
    .with_now_fn(Arc::new(move || now_ms))
}

fn broker(
    store: Arc<PgStore>,
    token_client: Arc<HttpOAuthTokenClient>,
    now_ms: i64,
) -> RegistryTokenBroker {
    RegistryTokenBroker::new(store.clone(), store.clone(), store.clone())
        .with_oauth_refresh(OAuthRefreshRuntime::new(store, token_client).with_expiry_margin_ms(0))
        .with_now_fn(Arc::new(move || now_ms))
}

fn authorization_parameter(authorize_url: &str, name: &str) -> anyhow::Result<String> {
    Url::parse(authorize_url)?
        .query_pairs()
        .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
        .ok_or_else(|| anyhow::anyhow!("authorization URL is missing {name}"))
}

fn assert_authorization_request(authorize_url: &str, resource: &str) -> anyhow::Result<()> {
    assert_eq!(
        authorization_parameter(authorize_url, "resource")?,
        resource
    );
    assert_eq!(
        authorization_parameter(authorize_url, "code_challenge_method")?,
        "S256"
    );
    let scopes = authorization_parameter(authorize_url, "scope")?;
    assert!(scopes.split_whitespace().any(|scope| scope == "tools.read"));
    assert!(
        scopes
            .split_whitespace()
            .any(|scope| scope == "offline_access")
    );
    Ok(())
}

async fn live_store() -> anyhow::Result<Arc<PgStore>> {
    let database_url = env::var("LIGHTSPEED_TEST_POSTGRES_URL")
        .map_err(|_| anyhow::anyhow!("LIGHTSPEED_TEST_POSTGRES_URL must be set; run ./dev.sh infra and source scripts/dev/env.sh"))?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&database_url)
        .await?;
    PgStore::migrate(&pool).await?;
    let store = Arc::new(PgStore::new(
        pool,
        PgStoreConfig::new(Uuid::new_v4()).with_secrets_master_key(random_master_key()),
    ));
    store.ensure_universe().await?;
    Ok(store)
}

fn random_master_key() -> SecretsMasterKey {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    SecretsMasterKey::from_bytes(bytes)
}

async fn start_oauth_mcp_fixture() -> anyhow::Result<(
    Arc<Mutex<OAuthMcpFixtureState>>,
    String,
    tokio::task::JoinHandle<()>,
)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let issuer = format!("http://{address}");
    let resource = format!("{issuer}/mcp");
    let state = Arc::new(Mutex::new(OAuthMcpFixtureState {
        issuer,
        resource: resource.clone(),
        ..OAuthMcpFixtureState::default()
    }));
    let app = Router::new()
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata),
        )
        .route("/register", post(register_client))
        .route("/token", post(token_endpoint))
        .route("/mcp", post(mcp_endpoint).delete(mcp_shutdown))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve OAuth/MCP fixture");
    });
    Ok((state, resource, task))
}

async fn protected_resource_metadata(
    State(state): State<Arc<Mutex<OAuthMcpFixtureState>>>,
) -> Json<Value> {
    let state = state.lock().await;
    Json(json!({
        "resource": state.resource,
        "authorization_servers": [state.issuer],
        "scopes_supported": ["tools.read", "tools.write"]
    }))
}

async fn authorization_server_metadata(
    State(state): State<Arc<Mutex<OAuthMcpFixtureState>>>,
) -> Json<Value> {
    let state = state.lock().await;
    Json(json!({
        "issuer": state.issuer,
        "authorization_endpoint": format!("{}/authorize", state.issuer),
        "token_endpoint": format!("{}/token", state.issuer),
        "registration_endpoint": format!("{}/register", state.issuer),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "authorization_response_iss_parameter_supported": true,
        "scopes_supported": ["tools.read", "tools.write", "offline_access"]
    }))
}

async fn register_client(
    State(state): State<Arc<Mutex<OAuthMcpFixtureState>>>,
    Json(document): Json<Value>,
) -> impl IntoResponse {
    let redirect_uris = document["redirect_uris"].clone();
    state.lock().await.registration_documents.push(document);
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": "lightspeed-live-client",
            "token_endpoint_auth_method": "none",
            "redirect_uris": redirect_uris
        })),
    )
}

async fn token_endpoint(
    State(state): State<Arc<Mutex<OAuthMcpFixtureState>>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let mut state = state.lock().await;
    state.token_forms.push(form.clone());
    if form.get("resource") != Some(&state.resource)
        || form.get("client_id").map(String::as_str) != Some("lightspeed-live-client")
    {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_target");
    }
    match form.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            if form.get("code").map(String::as_str) != Some("valid-code")
                || form.get("code_verifier").is_none_or(String::is_empty)
            {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
            }
            Json(json!({
                "access_token": "access-1",
                "token_type": "Bearer",
                "expires_in": 1,
                "refresh_token": "refresh-1",
                "scope": "tools.read offline_access"
            }))
            .into_response()
        }
        Some("refresh_token") => {
            state.refresh_count += 1;
            if state.reject_refresh {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
            }
            let expected = if state.refresh_count == 1 {
                "refresh-1"
            } else {
                "refresh-2"
            };
            if form.get("refresh_token").map(String::as_str) != Some(expected) {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
            }
            Json(json!({
                "access_token": "access-2",
                "token_type": "Bearer",
                "expires_in": 1,
                "refresh_token": "refresh-2",
                "scope": "tools.read offline_access"
            }))
            .into_response()
        }
        _ => oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type"),
    }
}

fn oauth_error(status: StatusCode, error: &str) -> Response {
    (status, Json(json!({"error": error}))).into_response()
}

async fn mcp_endpoint(
    State(state): State<Arc<Mutex<OAuthMcpFixtureState>>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let mut state = state.lock().await;
    state.mcp_authorizations.push(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
    );
    if state.require_additional_scope {
        let mut response = StatusCode::UNAUTHORIZED.into_response();
        response.headers_mut().insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(
                "Bearer error=\"insufficient_scope\", scope=\"tools.read tools.write\"",
            ),
        );
        return response;
    }
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer access-2")
    {
        let mut response = StatusCode::UNAUTHORIZED.into_response();
        response.headers_mut().insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer error=\"invalid_token\""),
        );
        return response;
    }
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    match request.get("method").and_then(Value::as_str) {
        Some("initialize") => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "oauth-live", "version": "1"}
            }
        }))
        .into_response(),
        Some("notifications/initialized") => StatusCode::ACCEPTED.into_response(),
        Some("tools/list") => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [{
                    "name": "lookup",
                    "description": "OAuth-protected lookup",
                    "inputSchema": {"type": "object"}
                }]
            }
        }))
        .into_response(),
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn mcp_shutdown() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn discover_tools(endpoint: &str, token: &str) -> anyhow::Result<Vec<String>> {
    let http = reqwest::Client::new();
    let initialized = http
        .post(endpoint)
        .bearer_auth(token)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "lightspeed-live-test", "version": "1"}
            }
        }))
        .send()
        .await?;
    anyhow::ensure!(initialized.status().is_success(), "MCP initialize failed");
    let listed = http
        .post(endpoint)
        .bearer_auth(token)
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
        .send()
        .await?;
    anyhow::ensure!(listed.status().is_success(), "MCP tools/list failed");
    let document: Value = listed.json().await?;
    let names = document
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    Ok(names)
}
