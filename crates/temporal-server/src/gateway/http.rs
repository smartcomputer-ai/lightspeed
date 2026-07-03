use std::{net::SocketAddr, sync::Arc};

use api::{AgentApiError, JsonRpcRequest, JsonRpcResponse, dispatch_json_rpc};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, StatusCode},
    response::Html,
    routing::{get, post},
};
use serde::Deserialize;
use store_pg::PgStore;
use temporalio_client::Client;
use uuid::Uuid;

use crate::{
    config::{DeploymentStores, GatewayAuthMode, gateway_auth_mode_from_env},
    universe::{UniverseError, UniverseRuntime},
};

use super::{GatewayAgentApi, OAuthCallbackOutcome, connect_temporal};

pub const DEFAULT_GATEWAY_BIND: &str = "127.0.0.1:18080";
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Trusted-header tenant selector. Only honored in `trusted-header` auth mode,
/// where an upstream gateway owns authentication and injects it; other modes
/// reject requests carrying it so tenant claims cannot be smuggled past a
/// different resolution mode.
pub const UNIVERSE_HEADER: &str = "x-lightspeed-universe";

#[derive(Clone, Debug)]
pub struct GatewayServerConfig {
    pub bind: SocketAddr,
    pub task_queue: String,
    pub temporal_target: String,
    pub namespace: String,
    pub max_request_body_bytes: usize,
    /// Externally reachable base URL for the OAuth callback
    /// (`{base}/auth/callback`). Defaults to `http://{bind}`.
    pub public_base_url: Option<String>,
}

/// Per-request universe resolution for the HTTP edge.
///
/// The JSON-RPC API itself never carries a universe parameter: session-scoped
/// methods reach the right universe because the resolved service instance is
/// universe-bound, and registry/list methods implicitly scope to it.
enum UniverseResolution {
    /// One injected service instance (tests, single-universe embeddings).
    /// Behaves like `single` mode: the universe header is rejected.
    FixedApi { api: Arc<GatewayAgentApi> },
    /// Deployment runtime: per-request resolution through the universe
    /// registry, honoring the configured auth mode.
    Multi {
        mode: GatewayAuthMode,
        runtime: Arc<UniverseRuntime>,
        public_base_url: String,
    },
}

pub struct GatewayState {
    resolution: UniverseResolution,
}

impl GatewayState {
    /// Route every request to one existing service instance.
    pub fn for_api(api: Arc<GatewayAgentApi>) -> Self {
        Self {
            resolution: UniverseResolution::FixedApi { api },
        }
    }

    pub fn multi(
        mode: GatewayAuthMode,
        runtime: Arc<UniverseRuntime>,
        public_base_url: String,
    ) -> Self {
        Self {
            resolution: UniverseResolution::Multi {
                mode,
                runtime,
                public_base_url,
            },
        }
    }

    async fn api_for_request(
        &self,
        headers: &HeaderMap,
    ) -> Result<Arc<GatewayAgentApi>, AgentApiError> {
        match &self.resolution {
            UniverseResolution::FixedApi { api } => {
                reject_universe_header(headers)?;
                Ok(api.clone())
            }
            UniverseResolution::Multi { mode, runtime, .. } => {
                let (universe_id, create_missing) = match mode {
                    GatewayAuthMode::Single { universe_id } => {
                        reject_universe_header(headers)?;
                        (*universe_id, true)
                    }
                    GatewayAuthMode::TrustedHeader { auto_create } => {
                        (universe_from_header(headers)?, *auto_create)
                    }
                };
                let state = runtime
                    .state_for(universe_id, create_missing)
                    .await
                    .map_err(map_universe_error)?;
                Ok(state.api.clone())
            }
        }
    }
}

fn reject_universe_header(headers: &HeaderMap) -> Result<(), AgentApiError> {
    if headers.contains_key(UNIVERSE_HEADER) {
        return Err(AgentApiError::invalid_request(format!(
            "{UNIVERSE_HEADER} is not accepted in this auth mode"
        )));
    }
    Ok(())
}

/// Fail closed: in `trusted-header` mode a request without the header is
/// rejected; there is never a fallback universe.
fn universe_from_header(headers: &HeaderMap) -> Result<Uuid, AgentApiError> {
    let value = headers.get(UNIVERSE_HEADER).ok_or_else(|| {
        AgentApiError::invalid_request(format!("missing required {UNIVERSE_HEADER} header"))
    })?;
    let value = value.to_str().map_err(|_| {
        AgentApiError::invalid_request(format!("invalid {UNIVERSE_HEADER} header encoding"))
    })?;
    Uuid::parse_str(value.trim()).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid {UNIVERSE_HEADER} header: {error}"))
    })
}

fn map_universe_error(error: UniverseError) -> AgentApiError {
    match error {
        UniverseError::Unknown { universe_id } => {
            AgentApiError::not_found(format!("unknown universe: {universe_id}"))
        }
        UniverseError::Runtime(error) => AgentApiError::internal(error.to_string()),
    }
}

pub async fn serve_gateway(config: GatewayServerConfig) -> anyhow::Result<()> {
    let mode = gateway_auth_mode_from_env()?;
    let client = connect_temporal(&config.temporal_target, &config.namespace).await?;
    let stores = DeploymentStores::from_env().await?;
    let public_base_url = public_base_url_or_default(&config);
    let runtime = Arc::new(UniverseRuntime::new(
        client,
        config.task_queue.clone(),
        Some(public_base_url.clone()),
        stores,
    ));
    prewarm_single_universe(&mode, &runtime).await?;
    let state = Arc::new(GatewayState::multi(mode, runtime, public_base_url));
    let app = gateway_router(state, config.max_request_body_bytes);
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(target: "temporal_server", bind = %config.bind, "gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// In `single` mode, build the pinned universe's state at startup so
/// misconfiguration fails the process instead of the first request. This also
/// preserves the pre-P90 behavior of creating the configured universe row.
pub async fn prewarm_single_universe(
    mode: &GatewayAuthMode,
    runtime: &Arc<UniverseRuntime>,
) -> anyhow::Result<()> {
    if let GatewayAuthMode::Single { universe_id } = mode {
        runtime.state_for(*universe_id, true).await?;
    }
    Ok(())
}

/// Single-instance gateway over an injected client/store (tests and
/// single-universe embeddings). The full multi-universe path is
/// [`serve_gateway`].
pub async fn serve_gateway_with_client_store(
    client: Client,
    store: Arc<PgStore>,
    config: GatewayServerConfig,
) -> anyhow::Result<()> {
    let public_base_url = public_base_url_or_default(&config);
    let api = Arc::new(
        GatewayAgentApi::builder(client, store)
            .with_task_queue(config.task_queue)
            .with_public_base_url(public_base_url)
            .build(),
    );
    let state = Arc::new(GatewayState::for_api(api));
    let app = gateway_router(state, config.max_request_body_bytes);
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(target: "temporal_server", bind = %config.bind, "gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}

pub fn public_base_url_or_default(config: &GatewayServerConfig) -> String {
    config
        .public_base_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", config.bind))
}

pub fn gateway_router(state: Arc<GatewayState>, max_request_body_bytes: usize) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/rpc", post(rpc))
        .route("/auth/callback", get(oauth_callback))
        .route("/auth/client-metadata.json", get(cimd_document))
        .layer(DefaultBodyLimit::max(max_request_body_bytes))
        .with_state(state)
}

/// Client ID Metadata Document (draft-ietf-oauth-client-id-metadata-document):
/// authorization servers fetch this to resolve Lightspeed's CIMD client id.
/// Deployment-scoped: it depends only on the public base URL, not a universe.
async fn cimd_document(State(state): State<Arc<GatewayState>>) -> Json<serde_json::Value> {
    match &state.resolution {
        UniverseResolution::FixedApi { api } => Json(api.cimd_document()),
        UniverseResolution::Multi {
            public_base_url, ..
        } => Json(super::service::cimd_document_for(public_base_url)),
    }
}

async fn rpc(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    let api = match state.api_for_request(&headers).await {
        Ok(api) => api,
        Err(error) => {
            return Json(JsonRpcResponse::failure(request.id, error.into()));
        }
    };
    Json(dispatch_json_rpc(api.as_ref(), request).await)
}

/// Query parameters of the OAuth authorization callback (RFC 6749 §4.1.2).
/// `code` is a one-time secret credential; this handler must never log it.
#[derive(Deserialize)]
struct OAuthCallbackQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

async fn oauth_callback(
    State(state): State<Arc<GatewayState>>,
    Query(query): Query<OAuthCallbackQuery>,
) -> (StatusCode, Html<String>) {
    let callback = auth::AuthCallback {
        state: query.state.unwrap_or_default(),
        code: query.code.map(auth::SecretValue::new),
        error: query.error,
        error_description: query.error_description,
    };
    // The callback is hit by external authorization servers and carries no
    // tenant header; its universe resolves from server-side flow state (the
    // hashed `state` parameter), never from request-supplied values.
    let api = match &state.resolution {
        UniverseResolution::FixedApi { api } => api.clone(),
        UniverseResolution::Multi { runtime, .. } => {
            let state_hash = auth::state_hash(&callback.state);
            match store_pg::find_auth_flow_universe(runtime.stores().pool(), &state_hash).await {
                Ok(Some(universe_id)) => match runtime.state_for(universe_id, false).await {
                    Ok(universe) => universe.api.clone(),
                    Err(error) => {
                        tracing::error!(target: "temporal_server", %error, "oauth callback universe resolution failed");
                        return callback_failure_page();
                    }
                },
                Ok(None) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        callback_page(
                            "Invalid authorization callback",
                            "The authorization state is unknown or expired. Start a new login and try again.",
                        ),
                    );
                }
                Err(error) => {
                    tracing::error!(target: "temporal_server", %error, "oauth callback flow lookup failed");
                    return callback_failure_page();
                }
            }
        }
    };
    match api.complete_oauth_callback(callback).await {
        OAuthCallbackOutcome::Completed { grant_id } => (
            StatusCode::OK,
            callback_page(
                "Authorization complete",
                &format!(
                    "Lightspeed stored the credential as grant {}. You can close this window.",
                    html_escape(&grant_id)
                ),
            ),
        ),
        OAuthCallbackOutcome::Failed { message } => (
            StatusCode::OK,
            callback_page(
                "Authorization failed",
                &format!(
                    "The authorization did not complete: {}. You can close this window and retry with a new login.",
                    html_escape(&message)
                ),
            ),
        ),
        OAuthCallbackOutcome::Rejected { message } => (
            StatusCode::BAD_REQUEST,
            callback_page(
                "Invalid authorization callback",
                &format!(
                    "{}. Start a new login and try again.",
                    html_escape(&message)
                ),
            ),
        ),
    }
}

fn callback_failure_page() -> (StatusCode, Html<String>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        callback_page(
            "Authorization failed",
            "Lightspeed could not process the authorization callback. Start a new login and try again.",
        ),
    )
}

fn callback_page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font-family: system-ui, sans-serif; max-width: 40rem; margin: 4rem auto;\">\
         <h1>{title}</h1><p>{body}</p></body></html>"
    ))
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::AgentApiErrorKind;

    fn headers_with_universe(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(UNIVERSE_HEADER, value.parse().expect("header value"));
        headers
    }

    #[test]
    fn universe_header_resolves_a_valid_uuid() {
        let universe_id = Uuid::parse_str("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f").expect("uuid");
        let headers = headers_with_universe(&universe_id.to_string());
        assert_eq!(universe_from_header(&headers).expect("resolve"), universe_id);
    }

    #[test]
    fn trusted_header_mode_fails_closed_without_the_header() {
        // No header never falls back to a default universe.
        let error = universe_from_header(&HeaderMap::new()).expect_err("must fail closed");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
    }

    #[test]
    fn universe_header_rejects_non_uuid_values() {
        let error =
            universe_from_header(&headers_with_universe("not-a-uuid")).expect_err("must reject");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
    }

    #[test]
    fn non_header_modes_reject_universe_header_smuggling() {
        // `single` mode (and any fixed-instance gateway) must not silently
        // ignore a tenant claim it does not honor.
        let headers = headers_with_universe("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f");
        let error = reject_universe_header(&headers).expect_err("must reject");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
        assert!(reject_universe_header(&HeaderMap::new()).is_ok());
    }

    #[test]
    fn unknown_universe_maps_to_not_found() {
        let error = map_universe_error(UniverseError::Unknown {
            universe_id: Uuid::nil(),
        });
        assert_eq!(error.kind, AgentApiErrorKind::NotFound);
    }
}
