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
    control::target::TargetOwner,
    ids::{BindingId, EnvironmentId, IncarnationId, ProviderTargetId, UniverseId},
    limits::MAX_MESSAGE_BYTES,
    routes::{CONNECT_PATH, CanonicalOrigin, DATA_PATH, canonical_origin, provider_data_route},
};
use environments::{EndpointScheme, EnvironmentEndpoint};
use futures_util::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use store_pg::PgApiKeyStore;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config, connect_async_with_config,
    tungstenite::{
        Message as UpstreamMessage, client::IntoClientRequest as _,
        http::HeaderValue as UpstreamHeaderValue, protocol::WebSocketConfig,
    },
};
use uuid::Uuid;

use crate::{
    config::{DeploymentStores, GatewayAuthMode, gateway_auth_mode_from_env},
    environments::gateway::{ROUTE_PATH_PREFIX, RouteKey, bearer_matches, close_message},
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
    /// Externally reachable base URL: the OAuth callback
    /// (`{base}/auth/callback`) and the origin registered daemons sign.
    /// Defaults to `http://{bind}`, which only a loopback bind may rely on.
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
    /// The origin daemons sign registrations for: the canonical origin of
    /// the public base URL. Absent when that URL has no WebSocket origin;
    /// the connect route then refuses every daemon.
    registration_origin: Option<CanonicalOrigin>,
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
        let registration_origin = registration_origin_of(api.public_base_url());
        Self {
            resolution: UniverseResolution::FixedApi { api },
            registrations: Arc::new(RegisteredConnections::new()),
            registration_origin,
        }
    }

    pub(super) fn registrations(&self) -> &Arc<RegisteredConnections> {
        &self.registrations
    }

    pub(super) fn registration_origin(&self) -> Option<&CanonicalOrigin> {
        self.registration_origin.as_ref()
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
                    &params.daemon_public_key.to_hex(),
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
                if let Some(universe_id) = store_pg::find_registered_environment_universe(
                    pool,
                    &params.daemon_public_key.to_hex(),
                )
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
        let registration_origin = registration_origin_of(&public_base_url);
        Self {
            resolution: UniverseResolution::Multi {
                mode,
                runtime,
                public_base_url,
                api_keys,
                deployment,
            },
            registrations: Arc::new(RegisteredConnections::new()),
            registration_origin,
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
        if request.method == api::METHOD_INITIALIZE {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            return match serde_json::from_value::<api::InitializeParams>(params) {
                Ok(_) => JsonRpcResponse::success(
                    request.id,
                    serde_json::to_value(request_context::initialize_response(
                        context.caller_access(),
                    ))
                    .expect("serializable handshake"),
                ),
                Err(error) => JsonRpcResponse::failure(
                    request.id,
                    AgentApiError::invalid_request(error.to_string()).into(),
                ),
            };
        }
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

    fn deployment_store(&self) -> Arc<store_pg::PgStore> {
        match &self.resolution {
            UniverseResolution::FixedApi { api } => api.store().clone(),
            UniverseResolution::Multi { runtime, .. } => runtime.stores().store_for(Uuid::nil()),
        }
    }

    async fn environment_provider(
        &self,
        provider_id: &environments::EnvironmentProviderId,
    ) -> Result<environments::EnvironmentProviderRecord, environments::EnvironmentRegistryError>
    {
        environments::EnvironmentProviderStore::read_provider(
            self.deployment_store().as_ref(),
            provider_id,
        )
        .await
    }

    async fn environment_provider_credential(
        &self,
        provider_id: &environments::EnvironmentProviderId,
    ) -> Result<auth::SecretValue, environments::EnvironmentRegistryError> {
        environments::EnvironmentProviderStore::read_provider_credential(
            self.deployment_store().as_ref(),
            provider_id,
        )
        .await
    }
}

/// The registration signature origin of a public base URL, if it has one.
fn registration_origin_of(public_base_url: &str) -> Option<CanonicalOrigin> {
    match canonical_origin(public_base_url) {
        Ok(origin) => Some(origin),
        Err(error) => {
            tracing::error!(
                target: "temporal_runtime",
                %error,
                public_base_url,
                "the public base URL has no WebSocket origin; daemon registration is refused"
            );
            None
        }
    }
}

/// The origin registered daemons must sign for, checked once at startup by
/// a process that serves the registration route: the canonical origin of
/// `LIGHTSPEED_PUBLIC_BASE_URL`. Without that variable the default
/// `http://{bind}` is the true origin only on a loopback bind, so any other
/// bind refuses to start rather than reject every remote daemon.
pub fn registration_origin(
    public_base_url: Option<&str>,
    bind: SocketAddr,
) -> anyhow::Result<CanonicalOrigin> {
    let Some(public_base_url) = public_base_url else {
        if !bind.ip().is_loopback() {
            anyhow::bail!(
                "LIGHTSPEED_PUBLIC_BASE_URL is required to serve environment registration on {bind}: \
                 daemons sign the gateway origin they dial, and only that URL names it"
            );
        }
        return Ok(canonical_origin(&format!("http://{bind}"))?);
    };
    canonical_origin(public_base_url).map_err(|error| {
        anyhow::anyhow!("LIGHTSPEED_PUBLIC_BASE_URL has no WebSocket origin: {error}")
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
    let stores = DeploymentStores::from_env()
        .await?
        .with_blob_cache(crate::config::blob_cache_from_env()?);
    registration_origin(config.public_base_url.as_deref(), config.bind)?;
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
    tracing::info!(target: "temporal_runtime", bind = %config.bind, "gateway listening");
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
                "/hooks/bots/{universe}/{bot}/{trigger}/{token}",
                post(bot_webhook_ingest),
            );
    }
    if routes.environment {
        router = router
            .route(
                &format!("{ROUTE_PATH_PREFIX}/{{universe}}/{{environment}}/{{incarnation}}"),
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
    let upgrade = upgrade
        .max_message_size(MAX_MESSAGE_BYTES as usize)
        .max_frame_size(MAX_MESSAGE_BYTES as usize);
    match &environment.source {
        environments::EnvironmentSource::External { endpoint } => {
            let upstream =
                match dial_upstream(endpoint.as_str(), external_target(endpoint), None).await {
                    Ok(upstream) => upstream,
                    Err(failure) => return failure.respond(endpoint.as_str()),
                };
            let route = UpstreamRoute::External {
                endpoint: endpoint.clone(),
            };
            upgrade
                .on_upgrade(move |socket| proxy_upstream(state, key, route, socket, upstream))
                .into_response()
        }
        environments::EnvironmentSource::Provisioned {
            provider_id,
            binding_id,
        } => {
            let Some(target_id) = environment.incarnation.provider_target_id.clone() else {
                return StatusCode::CONFLICT.into_response();
            };
            let route = UpstreamRoute::Provider {
                provider_id: provider_id.clone(),
                binding_id: binding_id.clone(),
                target_id,
            };
            if authorize_upstream(&state, &key, &route).await.is_err() {
                return StatusCode::CONFLICT.into_response();
            }
            let (endpoint, credential) = match provider_route(&state, &key, &route).await {
                Ok(route) => route,
                Err(refusal) => return refusal.respond(),
            };
            let upstream = match dial_upstream(
                &endpoint,
                UpstreamTarget::Url,
                Some(credential.expose()),
            )
            .await
            {
                Ok(upstream) => upstream,
                Err(failure) => return failure.respond(&endpoint),
            };
            upgrade
                .on_upgrade(move |socket| proxy_upstream(state, key, route, socket, upstream))
                .into_response()
        }
        environments::EnvironmentSource::Registered { .. } => {
            if environment.status.is_closing_or_closed() {
                return StatusCode::CONFLICT.into_response();
            }
            let (daemon_socket, control_connection_id) = match registration::open_registered_route(
                &state, &key,
            )
            .await
            {
                Ok(paired) => paired,
                Err(status) => {
                    tracing::warn!(target: "temporal_runtime", environment = %key.environment_id, %status, "registered environment data route unavailable");
                    return status.into_response();
                }
            };
            upgrade
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

/// How long the gateway waits for a provider route or external envd to
/// accept the upstream WebSocket.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A provider that has not yet advertised its data routes is asked again
/// after this long.
const PROVIDER_ROUTES_RETRY_AFTER_SECS: u64 = 5;

/// The upstream a worker route is proxied to, and what keeps it
/// authorized.
enum UpstreamRoute {
    External {
        endpoint: EnvironmentEndpoint,
    },
    Provider {
        provider_id: environments::EnvironmentProviderId,
        binding_id: environments::EnvironmentProviderBindingId,
        target_id: ProviderTargetId,
    },
}

/// Why a provisioned target's data route cannot be dialed now.
enum ProviderRouteRefusal {
    /// The provider's controller has not advertised its data routes yet.
    NotAdvertised,
    Status(StatusCode),
}

impl ProviderRouteRefusal {
    fn respond(self) -> Response {
        match self {
            Self::NotAdvertised => (
                StatusCode::SERVICE_UNAVAILABLE,
                [(
                    header::RETRY_AFTER,
                    HeaderValue::from(PROVIDER_ROUTES_RETRY_AFTER_SECS),
                )],
            )
                .into_response(),
            Self::Status(status) => status.into_response(),
        }
    }
}

/// The provider data route for a provisioned target and the credential it
/// is dialed with. The route comes only from the base the controller
/// advertised; a provider that has not advertised one yet is asked to
/// retry, one without a credential is a configuration error.
async fn provider_route(
    state: &GatewayState,
    key: &RouteKey,
    route: &UpstreamRoute,
) -> Result<(String, auth::SecretValue), ProviderRouteRefusal> {
    let UpstreamRoute::Provider {
        provider_id,
        binding_id,
        target_id,
    } = route
    else {
        return Err(ProviderRouteRefusal::Status(StatusCode::BAD_GATEWAY));
    };
    let unavailable = |error: environments::EnvironmentRegistryError| match error {
        environments::EnvironmentRegistryError::Store { .. } => {
            tracing::warn!(target: "temporal_runtime", %error, provider = %provider_id, "provider record unavailable");
            ProviderRouteRefusal::Status(StatusCode::SERVICE_UNAVAILABLE)
        }
        error => {
            tracing::error!(target: "temporal_runtime", %error, provider = %provider_id, "provider data route is misconfigured");
            ProviderRouteRefusal::Status(StatusCode::BAD_GATEWAY)
        }
    };
    let provider = state
        .environment_provider(provider_id)
        .await
        .map_err(unavailable)?;
    let Some(base) = provider.data_route_base.as_ref() else {
        return Err(ProviderRouteRefusal::NotAdvertised);
    };
    let credential = state
        .environment_provider_credential(provider_id)
        .await
        .map_err(unavailable)?;
    let owner = target_owner(key, binding_id).map_err(|error| {
        tracing::error!(target: "temporal_runtime", %error, environment = %key.environment_id, "route identity is not a valid target owner");
        ProviderRouteRefusal::Status(StatusCode::BAD_REQUEST)
    })?;
    Ok((provider_data_route(base, &owner, target_id), credential))
}

fn target_owner(
    key: &RouteKey,
    binding_id: &environments::EnvironmentProviderBindingId,
) -> Result<TargetOwner, environment_protocol::ids::IdError> {
    Ok(TargetOwner {
        universe_id: UniverseId::new(key.universe_id.to_string())?,
        binding_id: BindingId::new(binding_id.as_str())?,
        environment_id: EnvironmentId::new(key.environment_id.as_str())?,
        incarnation_id: IncarnationId::new(key.incarnation_id.as_str())?,
    })
}

/// Where an upstream socket is dialed.
#[derive(Clone, Copy)]
enum UpstreamTarget<'a> {
    /// `ws://` or `wss://`, dialed as written.
    Url,
    /// A Unix socket path; the upgrade targets `/` with `Host: localhost`.
    Unix(&'a str),
}

fn external_target(endpoint: &EnvironmentEndpoint) -> UpstreamTarget<'_> {
    match (endpoint.scheme(), endpoint.unix_socket_path()) {
        (EndpointScheme::Unix, Some(path)) => UpstreamTarget::Unix(path),
        _ => UpstreamTarget::Url,
    }
}

enum Upstream {
    Tcp(Box<WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>),
    Unix(Box<WebSocketStream<tokio::net::UnixStream>>),
}

/// Why an upstream dial failed, and what the worker is told.
enum UpstreamFailure {
    /// Unreachable, slow or answering 5xx/429: the worker may retry.
    Unavailable(String),
    /// The upstream refused the upgrade for good (wrong credential, no such
    /// route) or the URL is unusable.
    Refused(String),
}

impl UpstreamFailure {
    fn respond(self, endpoint: &str) -> Response {
        match self {
            Self::Unavailable(error) => {
                tracing::warn!(target: "temporal_runtime", %error, %endpoint, "environment upstream unavailable");
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
            Self::Refused(error) => {
                tracing::error!(target: "temporal_runtime", %error, %endpoint, "environment upstream refused the connection");
                StatusCode::BAD_GATEWAY.into_response()
            }
        }
    }
}

fn upstream_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES as usize))
        .max_frame_size(Some(MAX_MESSAGE_BYTES as usize))
}

/// Dial an upstream data socket with the protocol's message limits, an
/// optional bearer credential and a connect bound.
async fn dial_upstream(
    endpoint: &str,
    target: UpstreamTarget<'_>,
    bearer: Option<&str>,
) -> Result<Upstream, UpstreamFailure> {
    let url = match target {
        UpstreamTarget::Url => endpoint,
        UpstreamTarget::Unix(_) => "ws://localhost/",
    };
    let mut request = url
        .into_client_request()
        .map_err(|error| UpstreamFailure::Refused(error.to_string()))?;
    if let Some(bearer) = bearer {
        let mut value =
            UpstreamHeaderValue::from_str(&format!("Bearer {bearer}")).map_err(|_| {
                UpstreamFailure::Refused("the credential is not a valid header value".to_owned())
            })?;
        value.set_sensitive(true);
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            value,
        );
    }
    let dialed = tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, async {
        match target {
            UpstreamTarget::Url => {
                connect_async_with_config(request, Some(upstream_config()), true)
                    .await
                    .map(|(socket, _)| Upstream::Tcp(Box::new(socket)))
                    .map_err(upstream_failure)
            }
            UpstreamTarget::Unix(path) => {
                let stream = tokio::net::UnixStream::connect(path)
                    .await
                    .map_err(|error| UpstreamFailure::Unavailable(error.to_string()))?;
                client_async_with_config(request, stream, Some(upstream_config()))
                    .await
                    .map(|(socket, _)| Upstream::Unix(Box::new(socket)))
                    .map_err(upstream_failure)
            }
        }
    })
    .await;
    dialed.unwrap_or_else(|_| {
        Err(UpstreamFailure::Unavailable(format!(
            "no upgrade within {} s",
            UPSTREAM_CONNECT_TIMEOUT.as_secs()
        )))
    })
}

fn upstream_failure(error: tokio_tungstenite::tungstenite::Error) -> UpstreamFailure {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Http(response) => {
            let status = response.status();
            let message = format!("upgrade refused with HTTP {status}");
            if status.is_server_error() || status.as_u16() == 429 {
                UpstreamFailure::Unavailable(message)
            } else {
                UpstreamFailure::Refused(message)
            }
        }
        Error::Url(error) => UpstreamFailure::Refused(error.to_string()),
        error => UpstreamFailure::Unavailable(error.to_string()),
    }
}

async fn authorize_upstream(
    state: &GatewayState,
    key: &RouteKey,
    route: &UpstreamRoute,
) -> anyhow::Result<()> {
    let api = state
        .api_for_daemon(key.universe_id)
        .await
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let environment_id = environments::EnvironmentId::try_new(key.environment_id.clone())?;
    let environment =
        environments::EnvironmentStore::read_environment(api.store().as_ref(), &environment_id)
            .await?;
    if environment.incarnation.incarnation_id.as_str() != key.incarnation_id
        || environment.status.is_closing_or_closed()
    {
        anyhow::bail!("environment route is stale or the environment is closing")
    }
    match (route, &environment.source) {
        (
            UpstreamRoute::External { endpoint },
            environments::EnvironmentSource::External { endpoint: assigned },
        ) if assigned == endpoint => Ok(()),
        (
            UpstreamRoute::Provider {
                provider_id,
                binding_id,
                target_id,
            },
            environments::EnvironmentSource::Provisioned {
                provider_id: assigned_provider,
                binding_id: assigned_binding,
            },
        ) if assigned_provider == provider_id
            && assigned_binding == binding_id
            && environment.incarnation.provider_target_id.as_ref() == Some(target_id) =>
        {
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
        _ => anyhow::bail!("environment route no longer matches its environment"),
    }
}

async fn proxy_upstream(
    state: Arc<GatewayState>,
    key: RouteKey,
    route: UpstreamRoute,
    worker: WebSocket,
    upstream: Upstream,
) {
    match upstream {
        Upstream::Tcp(socket) => proxy_frames(state, key, route, worker, *socket).await,
        Upstream::Unix(socket) => proxy_frames(state, key, route, worker, *socket).await,
    }
}

/// Pipe one worker route and its upstream frame for frame, pings and pongs
/// included, re-checking the route's authorization on an interval.
async fn proxy_frames<S>(
    state: Arc<GatewayState>,
    key: RouteKey,
    route: UpstreamRoute,
    mut worker: WebSocket,
    upstream: WebSocketStream<S>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut upstream_writer, mut upstream_reader) = upstream.split();
    let mut reauthorize = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = reauthorize.tick() => {
                if authorize_upstream(&state, &key, &route).await.is_err() {
                    let _ = worker.send(close_message("environment route authorization changed")).await;
                    break;
                }
            }
            message = worker.recv() => {
                let Some(Ok(message)) = message else { break };
                let close = matches!(message, Message::Close(_));
                let message = match message {
                    Message::Text(value) => UpstreamMessage::Text(value.to_string().into()),
                    Message::Binary(value) => UpstreamMessage::Binary(value),
                    Message::Ping(value) => UpstreamMessage::Ping(value),
                    Message::Pong(value) => UpstreamMessage::Pong(value),
                    Message::Close(_) => UpstreamMessage::Close(None),
                };
                if upstream_writer.send(message).await.is_err() || close { break }
            }
            message = upstream_reader.next() => {
                let Some(Ok(message)) = message else { break };
                let close = matches!(message, UpstreamMessage::Close(_));
                let message = match message {
                    UpstreamMessage::Text(value) => Some(Message::Text(value.to_string().into())),
                    UpstreamMessage::Binary(value) => Some(Message::Binary(value)),
                    UpstreamMessage::Ping(value) => Some(Message::Ping(value)),
                    UpstreamMessage::Pong(value) => Some(Message::Pong(value)),
                    UpstreamMessage::Close(_) => Some(Message::Close(None)),
                    UpstreamMessage::Frame(_) => None,
                };
                if let Some(message) = message
                    && worker.send(message).await.is_err()
                { break }
                if close { break }
            }
        }
    }
    // Whichever leg ended first, complete the WebSocket closing handshake on
    // both sides instead of dropping the opposite stream abruptly; a worker
    // that vanished without a close frame never tears envd down abruptly.
    let _ = upstream_writer.send(UpstreamMessage::Close(None)).await;
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
                    target: "temporal_runtime",
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
                        tracing::error!(target: "temporal_runtime", %error, "oauth callback universe resolution failed");
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
                    tracing::error!(target: "temporal_runtime", %error, "oauth callback flow lookup failed");
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
    fn provider_data_routes_come_from_the_advertised_base() {
        let key = RouteKey {
            universe_id: Uuid::parse_str("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f").unwrap(),
            environment_id: "environment_1".to_owned(),
            incarnation_id: "incarnation_1".to_owned(),
        };
        let owner = target_owner(
            &key,
            &environments::EnvironmentProviderBindingId::new("primary"),
        )
        .unwrap();
        let base =
            environment_protocol::routes::DataRouteBase::new("wss://provider.example/routes")
                .unwrap();
        assert_eq!(
            provider_data_route(
                &base,
                &owner,
                &ProviderTargetId::new("default/pod one").unwrap()
            ),
            "wss://provider.example/routes/6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f/primary/environment_1/incarnation_1/default%2Fpod%20one"
        );
    }

    #[test]
    fn upstream_refusals_split_into_retryable_and_permanent() {
        let refused = |status: u16| {
            upstream_failure(tokio_tungstenite::tungstenite::Error::Http(Box::new(
                tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(status)
                    .body(None)
                    .unwrap(),
            )))
        };
        assert!(matches!(refused(401), UpstreamFailure::Refused(_)));
        assert!(matches!(refused(404), UpstreamFailure::Refused(_)));
        assert!(matches!(refused(503), UpstreamFailure::Unavailable(_)));
        assert!(matches!(refused(429), UpstreamFailure::Unavailable(_)));
    }

    #[test]
    fn the_registration_origin_comes_from_the_public_base_url() {
        let loopback: SocketAddr = "127.0.0.1:18080".parse().unwrap();
        let public: SocketAddr = "0.0.0.0:18080".parse().unwrap();
        assert_eq!(
            registration_origin(None, loopback).unwrap().as_str(),
            "ws://127.0.0.1:18080"
        );
        assert!(
            registration_origin(None, public).is_err(),
            "a non-loopback bind needs the public base URL"
        );
        assert_eq!(
            registration_origin(Some("https://Lightspeed.example/base/"), public)
                .unwrap()
                .as_str(),
            "wss://lightspeed.example"
        );
        assert!(registration_origin(Some("lightspeed.example"), public).is_err());
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
