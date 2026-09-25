use std::{net::SocketAddr, sync::Arc, time::Duration};

use super::authentication;
use super::request_context::RequestContext;
use api::AccessScope;
use api::{
    AgentApiError, JsonRpcRequest, JsonRpcResponse, dispatch_deployment_json_rpc,
    dispatch_json_rpc, is_deployment_method,
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use environment_protocol::{
    gateway::{PROVIDER_DATA_PATH_PREFIX, ROUTE_PATH_PREFIX},
    registration::{CONNECT_PATH, DATA_PATH},
};
use futures_util::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use store_pg::PgApiKeyStore;
use tokio_tungstenite::{connect_async, tungstenite::Message as ProviderMessage};
use uuid::Uuid;

use crate::{
    config::{DeploymentStores, GatewayAuthMode, gateway_auth_mode_from_env},
    environments::gateway::{RouteKey, bearer_matches, close_message},
    universe::{UniverseError, UniverseRuntime},
};

use super::{
    GatewayAgentApi, GatewayDeploymentApi, OAuthCallbackOutcome, connect_temporal,
    registration::{self, RegisteredConnections},
    request_context,
};

pub const DEFAULT_GATEWAY_BIND: &str = "127.0.0.1:18080";
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

pub use super::authentication::{ACTOR_HEADER, UNIVERSE_HEADER};

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
        api_keys: PgApiKeyStore,
        deployment: Arc<GatewayDeploymentApi>,
    },
}

pub struct GatewayState {
    resolution: UniverseResolution,
    /// Live registered-daemon connections on this replica.
    registrations: Arc<RegisteredConnections>,
    /// Public base URL daemons are told to dial for data connections.
    public_base_url: String,
}

/// Which route families one HTTP listener serves, derived from the
/// process's roles: the `gateway` role owns the API surface, the
/// `environment-gateway` role owns the environment data plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GatewayRoutes {
    /// JSON-RPC, OAuth callbacks, and bot webhook ingest.
    pub api: bool,
    /// Worker environment routes plus the public daemon connect and data
    /// routes.
    pub environment: bool,
}

impl GatewayRoutes {
    pub const ALL: Self = Self {
        api: true,
        environment: true,
    };
}

impl GatewayState {
    /// Route every request to one existing service instance.
    pub fn for_api(api: Arc<GatewayAgentApi>) -> Self {
        let public_base_url = api.public_base_url().to_owned();
        Self {
            resolution: UniverseResolution::FixedApi { api },
            registrations: Arc::new(RegisteredConnections::new()),
            public_base_url,
        }
    }

    pub(super) fn registrations(&self) -> &Arc<RegisteredConnections> {
        &self.registrations
    }

    /// Tell daemons to dial a different public base than the general one.
    pub fn with_environment_public_url(mut self, url: Option<String>) -> Self {
        if let Some(url) = url {
            self.public_base_url = url;
        }
        self
    }

    /// Public data route for reverse-dialed daemon sockets.
    pub(super) fn registration_data_url(&self) -> String {
        registration::data_url(&self.public_base_url)
    }

    /// The universe a register request belongs to: the environment's
    /// universe for a known daemon, the key's universe for a first
    /// registration.
    pub(super) async fn registration_universe(
        &self,
        params: &environment_protocol::registration::RegisterParams,
    ) -> Result<Option<Uuid>, AgentApiError> {
        let by_key = params
            .registration_key
            .as_ref()
            .map(|secret| environments::registration_key_hash(secret.expose()));
        match &self.resolution {
            UniverseResolution::FixedApi { api } => {
                let store = api.store();
                let universe_id = store.config().universe_id;
                let known = environments::EnvironmentStore::read_environment_by_daemon_public_key(
                    store.as_ref(),
                    &params.daemon_public_key,
                )
                .await
                .map_err(|error| AgentApiError::internal(error.to_string()))?;
                if known.is_some() {
                    return Ok(Some(universe_id));
                }
                let Some(hash) = by_key else {
                    return Ok(None);
                };
                let key = environments::EnvironmentRegistrationKeyStore::resolve_registration_key(
                    store.as_ref(),
                    &hash,
                )
                .await
                .map_err(|error| AgentApiError::internal(error.to_string()))?;
                Ok(key.map(|_| universe_id))
            }
            UniverseResolution::Multi { runtime, .. } => {
                let pool = runtime.stores().pool();
                if let Some(universe_id) =
                    store_pg::find_registered_environment_universe(pool, &params.daemon_public_key)
                        .await
                        .map_err(|error| AgentApiError::internal(error.to_string()))?
                {
                    return Ok(Some(universe_id));
                }
                let Some(hash) = by_key else {
                    return Ok(None);
                };
                store_pg::find_registration_key_universe(pool, &hash)
                    .await
                    .map_err(|error| AgentApiError::internal(error.to_string()))
            }
        }
    }

    pub fn multi(
        mode: GatewayAuthMode,
        runtime: Arc<UniverseRuntime>,
        public_base_url: String,
    ) -> Self {
        let api_keys = PgApiKeyStore::new(runtime.stores().pool().clone());
        let deployment = Arc::new(GatewayDeploymentApi::new(runtime.clone()));
        Self {
            resolution: UniverseResolution::Multi {
                mode,
                runtime,
                public_base_url: public_base_url.clone(),
                api_keys,
                deployment,
            },
            registrations: Arc::new(RegisteredConnections::new()),
            public_base_url,
        }
    }

    /// Resolve the caller once: what it addresses, its key and its actor.
    async fn request_context(
        &self,
        headers: &HeaderMap,
        method: &str,
    ) -> Result<RequestContext, AgentApiError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| AgentApiError::internal(e.to_string()))?
            .as_millis() as u64;
        let universe_id = match &self.resolution {
            UniverseResolution::Multi {
                mode: GatewayAuthMode::Authenticated,
                api_keys,
                ..
            } => {
                return authentication::authenticate(api_keys, headers, method, now).await;
            }
            UniverseResolution::FixedApi { api } => api.universe_id(),
            UniverseResolution::Multi {
                mode: GatewayAuthMode::Single { universe_id },
                ..
            } => *universe_id,
        };
        // Development mode: no key and no actor, never a header claim.
        api::method_access(method).ok_or_else(authentication::unknown_method)?;
        authentication::reject_identity_headers(headers)?;
        Ok(RequestContext::local(if is_deployment_method(method) {
            AccessScope::Deployment
        } else {
            AccessScope::Universe { universe_id }
        }))
    }

    async fn dispatch(&self, context: &RequestContext, request: JsonRpcRequest) -> JsonRpcResponse {
        if is_deployment_method(&request.method) {
            return match &self.resolution {
                UniverseResolution::Multi { deployment, .. } => {
                    dispatch_deployment_json_rpc(deployment.as_ref(), request).await
                }
                UniverseResolution::FixedApi { .. } => JsonRpcResponse::failure(
                    request.id,
                    AgentApiError::invalid_request(
                        "deployment methods are not available on this gateway",
                    )
                    .into(),
                ),
            };
        }
        match self.api_for_request(context).await {
            Ok(api) => dispatch_json_rpc(api.as_ref(), request).await,
            Err(error) => JsonRpcResponse::failure(request.id, error.into()),
        }
    }

    async fn api_for_request(
        &self,
        context: &RequestContext,
    ) -> Result<Arc<GatewayAgentApi>, AgentApiError> {
        let AccessScope::Universe { universe_id } = context.scope else {
            return Err(AgentApiError::invalid_request("universe context required"));
        };
        match &self.resolution {
            UniverseResolution::FixedApi { api } => Ok(api.clone()),
            UniverseResolution::Multi { runtime, .. } => runtime
                .state_for(universe_id, false)
                .await
                .map(|state| state.api.clone())
                .map_err(map_universe_error),
        }
    }

    pub(super) async fn api_for_daemon(
        &self,
        universe_id: Uuid,
    ) -> Result<Arc<GatewayAgentApi>, AgentApiError> {
        match &self.resolution {
            UniverseResolution::FixedApi { api } => {
                if api.store().config().universe_id != universe_id {
                    return Err(AgentApiError::not_found("unknown universe"));
                }
                Ok(api.clone())
            }
            UniverseResolution::Multi { runtime, .. } => runtime
                .state_for(universe_id, false)
                .await
                .map(|state| state.api.clone())
                .map_err(map_universe_error),
        }
    }

    fn environment_gateway_token(&self) -> &str {
        match &self.resolution {
            UniverseResolution::FixedApi { api } => api.environment_gateway.deployment_token(),
            UniverseResolution::Multi { runtime, .. } => {
                runtime.environment_gateway().deployment_token()
            }
        }
    }

    async fn environment_provider(
        &self,
        provider_id: &environments::EnvironmentProviderId,
    ) -> Result<environments::EnvironmentProviderRecord, AgentApiError> {
        let store = match &self.resolution {
            UniverseResolution::FixedApi { api } => api.store().clone(),
            UniverseResolution::Multi { runtime, .. } => runtime.stores().store_for(Uuid::nil()),
        };
        environments::EnvironmentProviderStore::read_provider(store.as_ref(), provider_id)
            .await
            .map_err(|error| AgentApiError::rejected(error.to_string()))
    }
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
    let stores = DeploymentStores::from_env()
        .await?
        .with_blob_cache(crate::config::blob_cache_from_env()?);
    let public_base_url = public_base_url_or_default(&config);
    let runtime = Arc::new(UniverseRuntime::new(
        client,
        config.task_queue.clone(),
        Some(public_base_url.clone()),
        stores,
    )?);
    prewarm_single_universe(&mode, &runtime).await?;
    let reconciler = tokio::spawn(runtime.clone().run_environment_reconciler());
    let state = Arc::new(GatewayState::multi(mode, runtime, public_base_url));
    let app = gateway_router(state, config.max_request_body_bytes, GatewayRoutes::ALL);
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(target: "temporal_server", bind = %config.bind, "gateway listening");
    axum::serve(listener, app).await?;
    reconciler.abort();
    Ok(())
}

/// In `single` mode, build the pinned universe's state at startup so
/// misconfiguration fails the process instead of the first request. This also
/// preserves the legacy behavior of creating the configured universe row.
pub async fn prewarm_single_universe(
    mode: &GatewayAuthMode,
    runtime: &Arc<UniverseRuntime>,
) -> anyhow::Result<()> {
    if let GatewayAuthMode::Single { universe_id } = mode {
        runtime.state_for(*universe_id, true).await?;
    }
    Ok(())
}

pub fn public_base_url_or_default(config: &GatewayServerConfig) -> String {
    config
        .public_base_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", config.bind))
}

pub fn gateway_router(
    state: Arc<GatewayState>,
    max_request_body_bytes: usize,
    routes: GatewayRoutes,
) -> Router {
    let mut router = Router::new().route("/health", get(|| async { "ok" }));
    if routes.api {
        router = router
            .route("/rpc", post(rpc))
            .route("/auth/callback", get(oauth_callback))
            .route("/auth/client-metadata.json", get(cimd_document))
            .route(
                "/hooks/bots/:universe/:bot/:trigger/:token",
                post(bot_webhook_ingest),
            );
    }
    if routes.environment {
        router = router
            .route(
                &format!("{ROUTE_PATH_PREFIX}/:universe/:environment/:incarnation"),
                get(environment_route_upgrade),
            )
            .route(CONNECT_PATH, get(registration::connect_upgrade))
            .route(DATA_PATH, get(registration::data_upgrade));
    }
    router
        .layer(DefaultBodyLimit::max(max_request_body_bytes))
        .with_state(state)
}

/// Public webhook ingress for bot triggers. No RPC auth: the URL token is
/// the baseline credential (checked in constant time), an HMAC scheme adds
/// a signature over the raw body. Unknown and mismatched endpoints are all
/// 404 so the path cannot be probed.
async fn bot_webhook_ingest(
    State(state): State<Arc<GatewayState>>,
    Path((universe, bot, trigger, token)): Path<(String, String, String, String)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    use crate::bots::hooks::WebhookIngestOutcome;

    let Ok(universe_id) = Uuid::parse_str(&universe) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let api = match state.api_for_daemon(universe_id).await {
        Ok(api) => api,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let raw_headers: std::collections::BTreeMap<String, String> = headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect();
    let outcome = api
        .ingest_bot_webhook(&bot, &trigger, &token, raw_headers, &body)
        .await;
    let (status, payload) = match outcome {
        WebhookIngestOutcome::Admitted {
            event_id,
            duplicate,
        } => (
            StatusCode::ACCEPTED,
            serde_json::json!({ "eventId": event_id, "duplicate": duplicate }),
        ),
        WebhookIngestOutcome::Filtered { error } => (
            StatusCode::ACCEPTED,
            serde_json::json!({ "filtered": true, "error": error }),
        ),
        WebhookIngestOutcome::UnknownEndpoint => return StatusCode::NOT_FOUND.into_response(),
        WebhookIngestOutcome::Unauthorized { message } => (
            StatusCode::UNAUTHORIZED,
            serde_json::json!({ "error": message }),
        ),
        WebhookIngestOutcome::Gone => (
            StatusCode::GONE,
            serde_json::json!({ "error": "bot is closed" }),
        ),
        WebhookIngestOutcome::Disabled { message } => (
            StatusCode::CONFLICT,
            serde_json::json!({ "error": message }),
        ),
        WebhookIngestOutcome::Throttled { message } => (
            StatusCode::TOO_MANY_REQUESTS,
            serde_json::json!({ "error": message }),
        ),
        WebhookIngestOutcome::TooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            serde_json::json!({ "error": "webhook body exceeds the 1 MiB cap" }),
        ),
        WebhookIngestOutcome::SecretUnavailable { message } => (
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": message }),
        ),
        WebhookIngestOutcome::BadPayload { message } => (
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": message }),
        ),
        WebhookIngestOutcome::Failed { message } => (
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": message }),
        ),
    };
    (status, Json(payload)).into_response()
}

async fn environment_route_upgrade(
    State(state): State<Arc<GatewayState>>,
    Path((universe, environment, incarnation)): Path<(String, String, String)>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !bearer_matches(&headers, state.environment_gateway_token()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(universe_id) = Uuid::parse_str(&universe) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let key = RouteKey {
        universe_id,
        environment_id: environment,
        incarnation_id: incarnation,
    };
    let api = match state.api_for_daemon(universe_id).await {
        Ok(api) => api,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let environment_id = match environments::EnvironmentId::try_new(key.environment_id.clone()) {
        Ok(id) => id,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let environment = match environments::EnvironmentStore::read_environment(
        api.store().as_ref(),
        &environment_id,
    )
    .await
    {
        Ok(environment) => environment,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    if environment.incarnation.incarnation_id.as_str() != key.incarnation_id {
        return StatusCode::CONFLICT.into_response();
    }
    match &environment.source {
        environments::EnvironmentSource::External { connection } => {
            if connection.transport != environment_protocol::shared::EnvironmentTransport::WebSocket
            {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            let endpoint = daemon_data_endpoint(&connection.endpoint);
            let daemon_socket = match connect_async(&endpoint).await {
                Ok((socket, _)) => socket,
                Err(error) => {
                    tracing::warn!(target: "temporal_server", %error, %endpoint, "external environment connection failed");
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
            };
            upgrade
                .max_message_size(64 * 1024 * 1024)
                .on_upgrade(move |socket| {
                    proxy_external_route(state, key, endpoint, socket, daemon_socket)
                })
                .into_response()
        }
        environments::EnvironmentSource::Provisioned {
            provider_id,
            binding_id,
        } => {
            let Some(target_id) = environment.incarnation.provider_target_id.as_ref() else {
                return StatusCode::CONFLICT.into_response();
            };
            if authorize_provisioned_route(
                &state,
                &key,
                provider_id,
                binding_id,
                target_id.as_str(),
            )
            .await
            .is_err()
            {
                return StatusCode::CONFLICT.into_response();
            }
            let provider = match state.environment_provider(provider_id).await {
                Ok(provider) => provider,
                Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
            };
            let endpoint = match provider_data_endpoint(
                &provider.controller_connection.endpoint,
                &key,
                binding_id.as_str(),
                target_id.as_str(),
            ) {
                Ok(endpoint) => endpoint,
                Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
            };
            let provider_socket = match connect_async(&endpoint).await {
                Ok((socket, _)) => socket,
                Err(error) => {
                    tracing::warn!(target: "temporal_server", %error, %endpoint, "provider data connection failed");
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
            };
            let provider_id = provider_id.clone();
            let binding_id = binding_id.clone();
            let target_id = target_id.to_string();
            upgrade
                .max_message_size(64 * 1024 * 1024)
                .on_upgrade(move |socket| {
                    proxy_provider_route(
                        state,
                        key,
                        provider_id,
                        binding_id,
                        target_id,
                        socket,
                        provider_socket,
                    )
                })
                .into_response()
        }
        environments::EnvironmentSource::Registered { .. } => {
            if matches!(
                environment.status,
                environments::EnvironmentStatus::Closing | environments::EnvironmentStatus::Closed
            ) {
                return StatusCode::CONFLICT.into_response();
            }
            let (daemon_socket, control_connection_id) = match registration::open_registered_route(
                &state, &key,
            )
            .await
            {
                Ok(paired) => paired,
                Err(status) => {
                    tracing::warn!(target: "temporal_server", environment = %key.environment_id, %status, "registered environment data route unavailable");
                    return status.into_response();
                }
            };
            upgrade
                .max_message_size(64 * 1024 * 1024)
                .on_upgrade(move |socket| {
                    registration::proxy_registered_route(
                        state,
                        key,
                        control_connection_id,
                        socket,
                        daemon_socket,
                    )
                })
                .into_response()
        }
    }
}

fn provider_data_endpoint(
    controller_endpoint: &str,
    key: &RouteKey,
    binding_id: &str,
    target_id: &str,
) -> anyhow::Result<String> {
    let mut endpoint = url::Url::parse(controller_endpoint)?;
    let scheme = match endpoint.scheme() {
        "http" => "ws",
        "https" => "wss",
        "ws" => "ws",
        "wss" => "wss",
        other => anyhow::bail!("unsupported provider controller URL scheme: {other}"),
    };
    endpoint
        .set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("invalid provider controller URL scheme"))?;
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    let base_path = endpoint
        .path()
        .trim_end_matches('/')
        .strip_suffix("/control")
        .ok_or_else(|| anyhow::anyhow!("provider controller endpoint must end in /control"))?;
    endpoint.set_path(&format!("{base_path}{PROVIDER_DATA_PATH_PREFIX}"));
    endpoint
        .path_segments_mut()
        .map_err(|_| anyhow::anyhow!("provider controller URL cannot be a base URL"))?
        .pop_if_empty()
        .push(&key.universe_id.to_string())
        .push(binding_id)
        .push(&key.environment_id)
        .push(&key.incarnation_id)
        .push(target_id);
    Ok(endpoint.into())
}

fn daemon_data_endpoint(endpoint: &str) -> String {
    if let Some(rest) = endpoint.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if let Some(rest) = endpoint.strip_prefix("https://") {
        format!("wss://{rest}")
    } else {
        endpoint.to_owned()
    }
}

async fn authorize_external_route(
    state: &GatewayState,
    key: &RouteKey,
    endpoint: &str,
) -> anyhow::Result<()> {
    let api = state
        .api_for_daemon(key.universe_id)
        .await
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let environment_id = environments::EnvironmentId::try_new(key.environment_id.clone())?;
    let environment =
        environments::EnvironmentStore::read_environment(api.store().as_ref(), &environment_id)
            .await?;
    let environments::EnvironmentSource::External {
        connection: assigned_connection,
    } = &environment.source
    else {
        anyhow::bail!("external route no longer belongs to an external environment")
    };
    if assigned_connection.transport
        != environment_protocol::shared::EnvironmentTransport::WebSocket
        || daemon_data_endpoint(&assigned_connection.endpoint) != endpoint
        || environment.incarnation.incarnation_id.as_str() != key.incarnation_id
        || matches!(
            environment.status,
            environments::EnvironmentStatus::Closing | environments::EnvironmentStatus::Closed
        )
    {
        anyhow::bail!("external environment route is stale or no longer authorized")
    }
    Ok(())
}

async fn authorize_provisioned_route(
    state: &GatewayState,
    key: &RouteKey,
    provider_id: &environments::EnvironmentProviderId,
    binding_id: &environments::EnvironmentProviderBindingId,
    target_id: &str,
) -> anyhow::Result<()> {
    let api = state
        .api_for_daemon(key.universe_id)
        .await
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let environment_id = environments::EnvironmentId::try_new(key.environment_id.clone())?;
    let environment =
        environments::EnvironmentStore::read_environment(api.store().as_ref(), &environment_id)
            .await?;
    let environments::EnvironmentSource::Provisioned {
        provider_id: assigned_provider,
        binding_id: assigned_binding,
    } = &environment.source
    else {
        anyhow::bail!("provider data routes require a provisioned environment")
    };
    if assigned_provider != provider_id
        || assigned_binding != binding_id
        || environment.incarnation.incarnation_id.as_str() != key.incarnation_id
        || environment
            .incarnation
            .provider_target_id
            .as_ref()
            .map(|id| id.as_str())
            != Some(target_id)
        || matches!(
            environment.status,
            environments::EnvironmentStatus::Closing | environments::EnvironmentStatus::Closed
        )
    {
        anyhow::bail!("provider data route is stale or no longer owned")
    }
    let binding = environments::EnvironmentProviderBindingStore::read_provider_binding(
        api.store().as_ref(),
        key.universe_id,
        binding_id,
    )
    .await?;
    if binding.status != environments::EnvironmentProviderBindingStatus::Enabled
        || binding.provider_id != *provider_id
    {
        anyhow::bail!("provider binding is disabled or changed")
    }
    Ok(())
}

async fn proxy_provider_route(
    state: Arc<GatewayState>,
    key: RouteKey,
    provider_id: environments::EnvironmentProviderId,
    binding_id: environments::EnvironmentProviderBindingId,
    target_id: String,
    mut worker: WebSocket,
    provider: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let (mut provider_writer, mut provider_reader) = provider.split();
    let mut reauthorize = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = reauthorize.tick() => {
                if authorize_provisioned_route(
                    &state, &key, &provider_id, &binding_id, &target_id,
                ).await.is_err() {
                    let _ = worker.send(close_message("provider route authorization changed")).await;
                    break;
                }
            }
            message = worker.recv() => {
                let Some(Ok(message)) = message else { break };
                let close = matches!(message, Message::Close(_));
                let message = match message {
                    Message::Text(value) => ProviderMessage::Text(value.to_string().into()),
                    Message::Binary(value) => ProviderMessage::Binary(value.to_vec().into()),
                    Message::Ping(value) => ProviderMessage::Ping(value.to_vec().into()),
                    Message::Pong(value) => ProviderMessage::Pong(value.to_vec().into()),
                    Message::Close(_) => ProviderMessage::Close(None),
                };
                if provider_writer.send(message).await.is_err() || close { break }
            }
            message = provider_reader.next() => {
                let Some(Ok(message)) = message else { break };
                let close = matches!(message, ProviderMessage::Close(_));
                let message = match message {
                    ProviderMessage::Text(value) => Some(Message::Text(value.to_string())),
                    ProviderMessage::Binary(value) => Some(Message::Binary(value.to_vec())),
                    ProviderMessage::Ping(value) => Some(Message::Ping(value.to_vec())),
                    ProviderMessage::Pong(value) => Some(Message::Pong(value.to_vec())),
                    ProviderMessage::Close(_) => Some(Message::Close(None)),
                    ProviderMessage::Frame(_) => None,
                };
                if let Some(message) = message
                    && worker.send(message).await.is_err()
                { break }
                if close { break }
            }
        }
    }
    // Whichever leg ended first, complete the WebSocket closing handshake on
    // both sides instead of dropping the opposite TCP stream abruptly.
    let _ = provider_writer.send(ProviderMessage::Close(None)).await;
    let _ = worker.send(Message::Close(None)).await;
}

async fn proxy_external_route(
    state: Arc<GatewayState>,
    key: RouteKey,
    endpoint: String,
    mut worker: WebSocket,
    daemon: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let (mut daemon_writer, mut daemon_reader) = daemon.split();
    let mut reauthorize = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = reauthorize.tick() => {
                if authorize_external_route(&state, &key, &endpoint).await.is_err() {
                    let _ = worker.send(close_message("external route authorization changed")).await;
                    break;
                }
            }
            message = worker.recv() => {
                let Some(Ok(message)) = message else { break };
                let close = matches!(message, Message::Close(_));
                let message = match message {
                    Message::Text(value) => ProviderMessage::Text(value.to_string().into()),
                    Message::Binary(value) => ProviderMessage::Binary(value.to_vec().into()),
                    Message::Ping(value) => ProviderMessage::Ping(value.to_vec().into()),
                    Message::Pong(value) => ProviderMessage::Pong(value.to_vec().into()),
                    Message::Close(_) => ProviderMessage::Close(None),
                };
                if daemon_writer.send(message).await.is_err() || close { break }
            }
            message = daemon_reader.next() => {
                let Some(Ok(message)) = message else { break };
                let close = matches!(message, ProviderMessage::Close(_));
                let message = match message {
                    ProviderMessage::Text(value) => Some(Message::Text(value.to_string())),
                    ProviderMessage::Binary(value) => Some(Message::Binary(value.to_vec())),
                    ProviderMessage::Ping(value) => Some(Message::Ping(value.to_vec())),
                    ProviderMessage::Pong(value) => Some(Message::Pong(value.to_vec())),
                    ProviderMessage::Close(_) => Some(Message::Close(None)),
                    ProviderMessage::Frame(_) => None,
                };
                if let Some(message) = message
                    && worker.send(message).await.is_err()
                { break }
                if close { break }
            }
        }
    }
    // A worker activity may be cancelled or its client may disappear without
    // a close frame. Do not propagate that abrupt teardown to envd.
    let _ = daemon_writer.send(ProviderMessage::Close(None)).await;
    let _ = worker.send(Message::Close(None)).await;
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
) -> Response {
    let method = request.method.clone();
    let context = match state.request_context(&headers, &method).await {
        Ok(context) => context,
        Err(error) => {
            if error.kind != api::AgentApiErrorKind::Forbidden {
                tracing::warn!(
                    target: "temporal_server",
                    %method,
                    kind = ?error.kind,
                    "request refused before authentication"
                );
            }
            return no_store_json_rpc(JsonRpcResponse::failure(request.id, error.into()));
        }
    };
    let response =
        request_context::with_request_context(context.clone(), state.dispatch(&context, request))
            .await;
    no_store_json_rpc(if response_budget_exempt(&method) {
        response
    } else {
        enforce_response_budget(response)
    })
}

/// Full message projections and blob reads cannot be shortened with a smaller
/// page when a single message is large. Their payloads remain complete; list
/// responses retain the generic budget, and tools retain bounded previews.
/// MCP discovery has its own typed decoded-inventory limit.
fn response_budget_exempt(method: &str) -> bool {
    matches!(
        method,
        api::METHOD_BLOBS_READ
            | api::METHOD_MCP_SERVERS_TOOLS_DISCOVER
            | api::METHOD_SESSION_READ
            | api::METHOD_SESSION_EVENTS_READ
            | api::METHOD_SESSION_RUNS_READ
            | api::METHOD_SESSION_RUNS_START
            | api::METHOD_SESSION_RUNS_CANCEL
            | api::METHOD_SESSION_RUNS_APPROVALS_DECIDE
            | api::METHOD_SESSION_RUNS_STEER
            | api::METHOD_SESSION_ENVIRONMENTS_ACTIVATE
            | api::METHOD_SESSION_ENVIRONMENTS_DEACTIVATE
            | api::METHOD_SESSION_PROFILES_APPLY
    )
}

fn enforce_response_budget(response: JsonRpcResponse) -> JsonRpcResponse {
    const MAX_JSON_RPC_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
    if response.result.is_some()
        && serde_json::to_vec(&response)
            .is_ok_and(|bytes| bytes.len() > MAX_JSON_RPC_RESPONSE_BYTES)
    {
        JsonRpcResponse::failure(
            response.id,
            AgentApiError::response_too_large(format!(
                "serialized response exceeds {MAX_JSON_RPC_RESPONSE_BYTES} bytes; retry with a smaller page limit"
            ))
            .into(),
        )
    } else {
        response
    }
}

fn no_store_json_rpc(response: JsonRpcResponse) -> Response {
    let mut response = Json(response).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
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
    iss: Option<String>,
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
        issuer: query.iss,
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

    #[test]
    fn json_rpc_responses_disable_caching() {
        let response = no_store_json_rpc(JsonRpcResponse::success(
            api::RequestId::Number(1),
            serde_json::json!({ "secret": "sensitive" }),
        ));
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static("no-store"))
        );
    }

    #[test]
    fn full_message_views_and_bulk_reads_are_response_budget_exempt() {
        assert!(response_budget_exempt(api::METHOD_BLOBS_READ));
        assert!(response_budget_exempt(
            api::METHOD_MCP_SERVERS_TOOLS_DISCOVER
        ));
        assert!(response_budget_exempt(api::METHOD_SESSION_READ));
        assert!(response_budget_exempt(api::METHOD_SESSION_EVENTS_READ));
        assert!(response_budget_exempt(api::METHOD_SESSION_RUNS_READ));
        assert!(!response_budget_exempt(api::METHOD_SESSION_LIST));
        assert!(!response_budget_exempt(api::METHOD_SESSION_RUNS_LIST));
    }

    #[test]
    fn non_exempt_oversized_json_rpc_response_is_rejected() {
        let response = JsonRpcResponse::success(
            api::RequestId::Number(7),
            serde_json::json!({"text": "x".repeat(2 * 1024 * 1024)}),
        );
        let bounded = enforce_response_budget(response);
        assert!(bounded.result.is_none());
        assert_eq!(
            bounded
                .error
                .expect("response-too-large error")
                .data
                .expect("typed error data")
                .kind,
            AgentApiErrorKind::ResponseTooLarge
        );
    }

    #[test]
    fn provider_data_url_is_derived_and_path_segments_are_escaped() {
        let key = RouteKey {
            universe_id: Uuid::parse_str("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f").unwrap(),
            environment_id: "environment with space".to_owned(),
            incarnation_id: "incarnation/one".to_owned(),
        };
        let endpoint = provider_data_endpoint(
            "https://provider.example/control",
            &key,
            "primary",
            "target-one",
        )
        .unwrap();
        assert_eq!(
            endpoint,
            "wss://provider.example/routes/6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f/primary/environment%20with%20space/incarnation%2Fone/target-one"
        );
        let nested = provider_data_endpoint(
            "https://provider.example/services/incus/control",
            &key,
            "primary",
            "target-one",
        )
        .unwrap();
        assert!(nested.starts_with("wss://provider.example/services/incus/routes/"));
        assert!(provider_data_endpoint("wss://provider.example/custom", &key, "b", "t").is_err());
    }

    #[test]
    fn local_mode_rejects_every_remote_identity_claim() {
        for name in [
            UNIVERSE_HEADER,
            ACTOR_HEADER,
            "x-lightspeed-principal",
            "authorization",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(name, "untrusted".parse().unwrap());
            assert!(authentication::reject_identity_headers(&headers).is_err());
        }
        assert!(authentication::reject_identity_headers(&HeaderMap::new()).is_ok());
    }

    #[test]
    fn unknown_universe_maps_to_not_found() {
        let error = map_universe_error(UniverseError::Unknown {
            universe_id: Uuid::nil(),
        });
        assert_eq!(error.kind, AgentApiErrorKind::NotFound);
    }
}
