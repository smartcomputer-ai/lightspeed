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
    control::target::{GetTargetParams, PowerState, TargetOwner, TargetPhase, TargetRef},
    ids::{EnvironmentId, UniverseId},
    limits::{MAX_MESSAGE_BYTES, RETRY_AFTER_MAX_SECS},
    metadata::DescriptiveText,
    routes::{
        CONNECT_PATH, CanonicalOrigin, DATA_PATH, DataRouteRefusal, canonical_origin,
        parse_retry_after_ms, provider_data_route,
    },
};
use environments::{
    EndpointScheme, EnvironmentEndpoint, EnvironmentGoneReason, EnvironmentUse,
    EnvironmentWaitReason, WAIT_RETRY_AFTER_MS,
};
use futures_util::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use store_pg::PgApiKeyStore;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::watch,
};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config, connect_async_with_config,
    tungstenite::{
        Message as UpstreamMessage, client::IntoClientRequest as _,
        http::HeaderValue as UpstreamHeaderValue, protocol::WebSocketConfig,
    },
};
use uuid::Uuid;

use crate::{
    config::GatewayAuthMode,
    environments::{
        gateway::{NO_WAKE_HEADER, ROUTE_PATH_PREFIX, RouteKey, bearer_matches, close_message},
        signals::EnvironmentSignals,
    },
    universe::{UniverseError, UniverseRuntime},
};

use super::{
    GatewayAgentApi, GatewayDeploymentApi, OAuthCallbackOutcome,
    registration::{self, RegisteredConnections},
    request_context,
};

pub const DEFAULT_GATEWAY_BIND: &str = "127.0.0.1:18080";
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

pub use super::authentication::{ACTOR_HEADER, UNIVERSE_HEADER};

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
    /// The origins a daemon's registration signature may name: the
    /// canonical origin of the public base URL and any configured
    /// registration URLs. Empty when none of them has a WebSocket origin;
    /// the connect route then refuses every daemon.
    registration_origins: Vec<CanonicalOrigin>,
    /// Revocation of live environment routes and control connections.
    signals: Arc<EnvironmentSignals>,
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
        let registration_origins = registration_origin_of(api.public_base_url())
            .into_iter()
            .collect();
        let signals = api.environment_signals().clone();
        Self {
            resolution: UniverseResolution::FixedApi { api },
            registrations: Arc::new(RegisteredConnections::new()),
            registration_origins,
            signals,
        }
    }

    /// Replace the origins registration signatures are verified against,
    /// as [`registration_origins`] computed them at startup.
    pub fn with_registration_origins(mut self, origins: Vec<CanonicalOrigin>) -> Self {
        self.registration_origins = origins;
        self
    }

    pub(super) fn registrations(&self) -> &Arc<RegisteredConnections> {
        &self.registrations
    }

    pub(super) fn registration_origins(&self) -> &[CanonicalOrigin] {
        &self.registration_origins
    }

    pub(super) fn signals(&self) -> &Arc<EnvironmentSignals> {
        &self.signals
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
        let registration_origins = registration_origin_of(&public_base_url)
            .into_iter()
            .collect();
        let signals = runtime.environment_signals().clone();
        Self {
            resolution: UniverseResolution::Multi {
                mode,
                runtime,
                public_base_url,
                api_keys,
                deployment,
            },
            registrations: Arc::new(RegisteredConnections::new()),
            registration_origins,
            signals,
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
    ) -> Result<Option<auth::SecretValue>, environments::EnvironmentRegistryError> {
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

/// The origins registered daemons may sign for, computed once at startup by
/// a process that serves the registration route: the canonical origin of
/// `LIGHTSPEED_PUBLIC_BASE_URL`, then the origin of each
/// `LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS` entry (for example a
/// cluster-internal Service name daemons dial instead). Without a public
/// base URL the default `http://{bind}` is a true origin only on a loopback
/// bind; any other bind needs at least one configured origin, or the
/// process refuses to start rather than reject every remote daemon. A URL
/// without a WebSocket origin is a startup error.
pub fn registration_origins(
    public_base_url: Option<&str>,
    registration_urls: &[String],
    bind: SocketAddr,
) -> anyhow::Result<Vec<CanonicalOrigin>> {
    let mut origins = Vec::new();
    match public_base_url {
        Some(public_base_url) => {
            origins.push(canonical_origin(public_base_url).map_err(|error| {
                anyhow::anyhow!("LIGHTSPEED_PUBLIC_BASE_URL has no WebSocket origin: {error}")
            })?)
        }
        None if bind.ip().is_loopback() => {
            origins.push(canonical_origin(&format!("http://{bind}"))?);
        }
        None => {}
    }
    for url in registration_urls
        .iter()
        .map(|url| url.trim())
        .filter(|url| !url.is_empty())
    {
        let origin = canonical_origin(url).map_err(|error| {
            anyhow::anyhow!(
                "LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS entry {url:?} has no WebSocket origin: {error}"
            )
        })?;
        if !origins.contains(&origin) {
            origins.push(origin);
        }
    }
    if origins.is_empty() {
        anyhow::bail!(
            "LIGHTSPEED_PUBLIC_BASE_URL or LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS is required to serve \
             environment registration on {bind}: daemons sign the gateway origin they dial, and only \
             those URLs name it"
        );
    }
    Ok(origins)
}

fn map_universe_error(error: UniverseError) -> AgentApiError {
    match error {
        UniverseError::Unknown { universe_id } => {
            AgentApiError::not_found(format!("unknown universe: {universe_id}"))
        }
        UniverseError::Runtime(error) => AgentApiError::internal(error.to_string()),
    }
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
                &format!("{ROUTE_PATH_PREFIX}/{{universe}}/{{environment}}"),
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

/// `GET {ROUTE_PATH_PREFIX}/{universe}/{environment}`: a worker's data
/// route. The row's use gate decides first; a usable environment is then
/// dialed by its source. Every refusal the gate or a provider makes is a
/// typed [`DataRouteRefusal`]; a bodiless status means the gateway itself
/// could not reach or authorize the upstream.
async fn environment_route_upgrade(
    State(state): State<Arc<GatewayState>>,
    Path((universe, environment)): Path<(String, String)>,
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
    };
    let api = match state.api_for_daemon(universe_id).await {
        Ok(api) => api,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let environment_id = match environments::EnvironmentId::try_new(key.environment_id.clone()) {
        Ok(id) => id,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    // Subscribe before reading the row: a revocation that lands after the
    // read still ends the route.
    let revoked = state.signals().watch(universe_id, &key.environment_id);
    let environment = match environments::EnvironmentStore::read_environment(
        api.store().as_ref(),
        &environment_id,
    )
    .await
    {
        Ok(environment) => environment,
        Err(environments::EnvironmentRegistryError::NotFound { .. }) => {
            return refusal_response(&DataRouteRefusal::Gone {}, None);
        }
        Err(error) => {
            tracing::warn!(target: "temporal_runtime", %error, environment = %key.environment_id, "environment route could not read its environment");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    if let Some(refused) = gate_refusal(&environment.use_gate(now_ms())) {
        return refused;
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
            upgrade
                .on_upgrade(move |socket| proxy_upstream(revoked, socket, upstream))
                .into_response()
        }
        environments::EnvironmentSource::Provisioned(source) => {
            let (endpoint, credential) = match provider_route(&state, &api, &key, source).await {
                Ok(route) => route,
                Err(refused) => return *refused,
            };
            if headers.contains_key(NO_WAKE_HEADER)
                && let Some(refused) = refuse_unless_running(&api, &key, source).await
            {
                return refused;
            }
            let upstream = match dial_upstream(
                &endpoint,
                UpstreamTarget::Url,
                credential.as_ref().map(|credential| credential.expose()),
            )
            .await
            {
                Ok(upstream) => upstream,
                Err(failure) => return failure.respond(&endpoint),
            };
            upgrade
                .on_upgrade(move |socket| proxy_upstream(revoked, socket, upstream))
                .into_response()
        }
        environments::EnvironmentSource::Registered(_) => {
            let (daemon_socket, control_ended) = match registration::open_registered_route(
                &state, &key,
            )
            .await
            {
                Ok(paired) => paired,
                Err(refused) => {
                    tracing::warn!(target: "temporal_runtime", environment = %key.environment_id, status = %refused.status(), "registered environment data route unavailable");
                    return *refused;
                }
            };
            upgrade
                .on_upgrade(move |socket| {
                    registration::proxy_registered_route(
                        revoked,
                        control_ended,
                        socket,
                        daemon_socket,
                    )
                })
                .into_response()
        }
    }
}

/// The refusal a worker route answers for a gate that does not admit use,
/// or `None` to dial. Waking and booting never come from the gate itself:
/// only a provider answers them.
fn gate_refusal(gate: &EnvironmentUse) -> Option<Response> {
    let refusal = match gate {
        EnvironmentUse::Use => return None,
        EnvironmentUse::Wait {
            reason,
            retry_after_ms,
            ..
        } => {
            let refusal = match reason {
                EnvironmentWaitReason::Provisioning => DataRouteRefusal::Provisioning {},
                EnvironmentWaitReason::Disconnected => DataRouteRefusal::Disconnected {},
                EnvironmentWaitReason::Waking => DataRouteRefusal::Waking {},
                EnvironmentWaitReason::Booting => DataRouteRefusal::Booting {},
            };
            return Some(refusal_response(&refusal, Some(*retry_after_ms)));
        }
        EnvironmentUse::Gone { reason, message } => match reason {
            EnvironmentGoneReason::Closing | EnvironmentGoneReason::Closed => {
                DataRouteRefusal::Gone {}
            }
            EnvironmentGoneReason::Away => DataRouteRefusal::Away {},
            EnvironmentGoneReason::Failed => DataRouteRefusal::Failed {
                message: message
                    .as_deref()
                    .and_then(DescriptiveText::lossy)
                    .unwrap_or_else(|| {
                        DescriptiveText::new("the environment failed")
                            .expect("a fixed failure message is valid text")
                    }),
            },
        },
    };
    Some(refusal_response(&refusal, None))
}

/// A typed data-route refusal: its status, a JSON body tagged by `state`,
/// and for a retryable one `Retry-After` in whole seconds, rounded up and
/// kept within 1 to [`RETRY_AFTER_MAX_SECS`] (default
/// [`WAIT_RETRY_AFTER_MS`]).
pub(super) fn refusal_response(
    refusal: &DataRouteRefusal,
    retry_after_ms: Option<u64>,
) -> Response {
    let status = StatusCode::from_u16(refusal.status()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
    let mut response = (status, Json(refusal)).into_response();
    if refusal.is_retryable() {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from(retry_after_secs(retry_after_ms)),
        );
    }
    response
}

fn retry_after_secs(retry_after_ms: Option<u64>) -> u64 {
    retry_after_ms
        .unwrap_or(WAIT_RETRY_AFTER_MS)
        .div_ceil(1_000)
        .clamp(1, RETRY_AFTER_MAX_SECS)
}

/// How long the gateway waits for a provider route or external envd to
/// accept the upstream WebSocket; longer than a provider may hold a dial
/// while it wakes the target (`MAX_DATA_ROUTE_HOLD_MS`).
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const _: () = assert!(
    UPSTREAM_CONNECT_TIMEOUT.as_millis()
        > environment_protocol::limits::MAX_DATA_ROUTE_HOLD_MS as u128
);

/// The provider data route of a provisioned environment and the credential
/// it is dialed with (none when the provider has none). The universe's
/// binding of the provider must exist and be enabled; a target the
/// provider has not named yet is still provisioning.
async fn provider_route(
    state: &GatewayState,
    api: &GatewayAgentApi,
    key: &RouteKey,
    source: &environments::ProvisionedSource,
) -> Result<(String, Option<auth::SecretValue>), Box<Response>> {
    let provider_id = &source.provider_id;
    let unavailable = |error: environments::EnvironmentRegistryError| match error {
        environments::EnvironmentRegistryError::Store { .. } => {
            tracing::warn!(target: "temporal_runtime", %error, provider = %provider_id, "provider record unavailable");
            Box::new(StatusCode::SERVICE_UNAVAILABLE.into_response())
        }
        error => {
            tracing::error!(target: "temporal_runtime", %error, provider = %provider_id, "provider data route is misconfigured");
            Box::new(StatusCode::BAD_GATEWAY.into_response())
        }
    };
    match environments::EnvironmentProviderBindingStore::read_provider_binding(
        api.store().as_ref(),
        key.universe_id,
        provider_id,
    )
    .await
    {
        Ok(binding) if binding.enabled => {}
        Ok(_) | Err(environments::EnvironmentRegistryError::NotFound { .. }) => {
            return Err(Box::new(StatusCode::FORBIDDEN.into_response()));
        }
        Err(error) => return Err(unavailable(error)),
    }
    let Some(target_id) = source.provider_target_id.as_ref() else {
        return Err(Box::new(refusal_response(
            &DataRouteRefusal::Provisioning {},
            None,
        )));
    };
    let provider = state
        .environment_provider(provider_id)
        .await
        .map_err(unavailable)?;
    let base = provider.effective_data_route_base().map_err(unavailable)?;
    let credential = state
        .environment_provider_credential(provider_id)
        .await
        .map_err(unavailable)?;
    let owner = target_owner(key).map_err(|error| {
        tracing::error!(target: "temporal_runtime", %error, environment = %key.environment_id, "route identity is not a valid target owner");
        Box::new(StatusCode::BAD_REQUEST.into_response())
    })?;
    Ok((provider_data_route(&base, &owner, target_id), credential))
}

/// Bound of the provider check a no-wake dial makes.
const NO_WAKE_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// For a dial that must not wake its target ([`NO_WAKE_HEADER`]): `None`
/// when the provider reports the target active and running right now (or
/// active without power control), else the refusal. Nothing is dialed or
/// written: a powered-down target is a bodiless 412, and a provider that
/// cannot answer in time a bodiless 503.
async fn refuse_unless_running(
    api: &GatewayAgentApi,
    key: &RouteKey,
    source: &environments::ProvisionedSource,
) -> Option<Response> {
    let (Some(target_id), Ok(owner)) = (source.provider_target_id.clone(), target_owner(key))
    else {
        return Some(StatusCode::SERVICE_UNAVAILABLE.into_response());
    };
    let service = api.environment_service();
    let observed = tokio::time::timeout(NO_WAKE_CHECK_TIMEOUT, async {
        let controller = service
            .connect_provider_controller(&source.provider_id)
            .await
            .map_err(|error| error.to_string())?;
        let answer = controller
            .client
            .get_target(&GetTargetParams {
                target: TargetRef {
                    owner,
                    target_id: Some(target_id),
                },
            })
            .await
            .map_err(|error| error.to_string());
        controller.close().await;
        answer.map(|response| response.target)
    })
    .await;
    match observed {
        Ok(Ok(target))
            if target.phase == TargetPhase::Active
                && target
                    .power
                    .is_none_or(|power| power == PowerState::Running) =>
        {
            None
        }
        Ok(Ok(_)) => Some(StatusCode::PRECONDITION_FAILED.into_response()),
        Ok(Err(error)) => {
            tracing::debug!(target: "temporal_runtime", %error, environment = %key.environment_id, "a no-wake dial could not check its target's power");
            Some(StatusCode::SERVICE_UNAVAILABLE.into_response())
        }
        Err(_) => Some(StatusCode::SERVICE_UNAVAILABLE.into_response()),
    }
}

fn target_owner(key: &RouteKey) -> Result<TargetOwner, environment_protocol::ids::IdError> {
    Ok(TargetOwner {
        universe_id: UniverseId::new(key.universe_id.to_string())?,
        environment_id: EnvironmentId::new(key.environment_id.as_str())?,
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
#[derive(Debug)]
enum UpstreamFailure {
    /// The upstream answered a typed refusal: forwarded unchanged.
    Refusal {
        refusal: DataRouteRefusal,
        retry_after_ms: Option<u64>,
    },
    /// Unreachable, slow or answering 5xx/429 without a refusal body: the
    /// worker may retry (bodiless 503).
    Unavailable(String),
    /// The upstream refused the upgrade for good (wrong credential, no such
    /// route) or the URL is unusable (bodiless 502).
    Refused(String),
}

impl UpstreamFailure {
    fn respond(self, endpoint: &str) -> Response {
        match self {
            Self::Refusal {
                refusal,
                retry_after_ms,
            } => {
                tracing::info!(target: "temporal_runtime", %refusal, %endpoint, "environment upstream refused the dial");
                refusal_response(&refusal, retry_after_ms)
            }
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
            let retry_after_ms = response
                .headers()
                .get(tokio_tungstenite::tungstenite::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_retry_after_ms);
            let refusal = DataRouteRefusal::from_refused_upgrade(
                status.as_u16(),
                response.body().as_deref(),
                retry_after_ms,
            );
            if let Some(refusal) = refusal {
                return UpstreamFailure::Refusal {
                    refusal,
                    retry_after_ms,
                };
            }
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

async fn proxy_upstream(revoked: watch::Receiver<u64>, worker: WebSocket, upstream: Upstream) {
    match upstream {
        Upstream::Tcp(socket) => proxy_frames(revoked, worker, *socket).await,
        Upstream::Unix(socket) => proxy_frames(revoked, worker, *socket).await,
    }
}

/// Pipe one worker route and its upstream frame for frame, pings and pongs
/// included, until either side ends or the environment's routes are
/// revoked (close code 1008).
async fn proxy_frames<S>(
    mut revoked: watch::Receiver<u64>,
    mut worker: WebSocket,
    upstream: WebSocketStream<S>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut upstream_writer, mut upstream_reader) = upstream.split();
    loop {
        tokio::select! {
            // A dropped revocation sender fails closed like a revocation.
            _ = revoked.changed() => {
                let _ = worker.send(close_message("environment route revoked")).await;
                break;
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
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
    use environment_protocol::ids::ProviderTargetId;

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
    fn provider_data_routes_name_the_universe_environment_and_target() {
        let key = RouteKey {
            universe_id: Uuid::parse_str("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f").unwrap(),
            environment_id: "environment_1".to_owned(),
        };
        let owner = target_owner(&key).unwrap();
        let base =
            environment_protocol::routes::DataRouteBase::new("wss://provider.example/routes")
                .unwrap();
        assert_eq!(
            provider_data_route(
                &base,
                &owner,
                &ProviderTargetId::new("default/pod one").unwrap()
            ),
            "wss://provider.example/routes/6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f/environment_1/default%2Fpod%20one"
        );
    }

    fn rejected(
        status: u16,
        body: Option<&str>,
        retry_after: Option<&str>,
    ) -> tokio_tungstenite::tungstenite::Error {
        let mut response = tokio_tungstenite::tungstenite::http::Response::builder().status(status);
        if let Some(retry_after) = retry_after {
            response = response.header("retry-after", retry_after);
        }
        tokio_tungstenite::tungstenite::Error::Http(Box::new(
            response
                .body(body.map(|body| body.as_bytes().to_vec()))
                .unwrap(),
        ))
    }

    #[test]
    fn upstream_refusals_split_into_typed_retryable_and_permanent() {
        assert!(matches!(
            upstream_failure(rejected(401, None, None)),
            UpstreamFailure::Refused(_)
        ));
        assert!(matches!(
            upstream_failure(rejected(404, Some("not json"), None)),
            UpstreamFailure::Refused(_)
        ));
        assert!(matches!(
            upstream_failure(rejected(503, None, None)),
            UpstreamFailure::Unavailable(_)
        ));
        assert!(matches!(
            upstream_failure(rejected(429, None, None)),
            UpstreamFailure::Unavailable(_)
        ));
        let UpstreamFailure::Refusal {
            refusal,
            retry_after_ms,
        } = upstream_failure(rejected(503, Some(r#"{"state":"waking"}"#), Some("3")))
        else {
            panic!("a typed provider refusal is forwarded");
        };
        assert_eq!(refusal, DataRouteRefusal::Waking {});
        assert_eq!(retry_after_ms, Some(3_000));
        let UpstreamFailure::Refusal {
            refusal,
            retry_after_ms,
        } = upstream_failure(rejected(
            409,
            Some(r#"{"state":"failed","message":"disk lost"}"#),
            None,
        ))
        else {
            panic!("a failed target is forwarded");
        };
        assert_eq!(refusal.status(), 409);
        assert_eq!(retry_after_ms, None);
        let UpstreamFailure::Refusal { retry_after_ms, .. } =
            upstream_failure(rejected(503, Some(r#"{"state":"booting"}"#), Some("3600")))
        else {
            panic!("a booting target is forwarded");
        };
        assert_eq!(retry_after_ms, Some(RETRY_AFTER_MAX_SECS * 1_000));
    }

    #[test]
    fn a_refusal_whose_body_was_not_read_is_forwarded_by_its_status() {
        let forwarded = |status: u16, retry_after: Option<&str>| match upstream_failure(rejected(
            status,
            None,
            retry_after,
        )) {
            UpstreamFailure::Refusal {
                refusal,
                retry_after_ms,
            } => Some((refusal, retry_after_ms)),
            _ => None,
        };
        assert_eq!(
            forwarded(410, None),
            Some((DataRouteRefusal::Gone {}, None))
        );
        assert!(matches!(
            forwarded(409, None),
            Some((DataRouteRefusal::Failed { .. }, None))
        ));
        assert_eq!(
            forwarded(503, Some("2")),
            Some((DataRouteRefusal::Booting {}, Some(2_000)))
        );
        // A 503 without `Retry-After` names no wait: a bodiless 503.
        assert_eq!(forwarded(503, None), None);
    }

    fn body_of(response: Response) -> DataRouteRefusal {
        let bytes =
            futures::executor::block_on(axum::body::to_bytes(response.into_body(), usize::MAX))
                .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn refusals_carry_their_status_body_and_retry_hint() {
        let waking = refusal_response(&DataRouteRefusal::Waking {}, Some(2_500));
        assert_eq!(waking.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            waking.headers().get(header::RETRY_AFTER),
            Some(&HeaderValue::from(3_u64))
        );
        assert_eq!(
            waking.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
        assert_eq!(body_of(waking), DataRouteRefusal::Waking {});

        let defaulted = refusal_response(&DataRouteRefusal::Disconnected {}, None);
        assert_eq!(
            defaulted.headers().get(header::RETRY_AFTER),
            Some(&HeaderValue::from(WAIT_RETRY_AFTER_MS / 1_000))
        );
        assert_eq!(retry_after_secs(Some(0)), 1);
        assert_eq!(
            retry_after_secs(Some(10 * 60 * 1_000)),
            RETRY_AFTER_MAX_SECS
        );

        let gone = refusal_response(&DataRouteRefusal::Gone {}, Some(5_000));
        assert_eq!(gone.status(), StatusCode::GONE);
        assert!(
            gone.headers().get(header::RETRY_AFTER).is_none(),
            "only retryable refusals carry Retry-After"
        );
    }

    #[test]
    fn the_use_gate_maps_to_route_refusals() {
        assert!(gate_refusal(&EnvironmentUse::Use).is_none());
        let wait = |reason| EnvironmentUse::Wait {
            reason,
            retry_after_ms: 2_000,
            until_ms: None,
        };
        let gone = |reason, message: Option<&str>| EnvironmentUse::Gone {
            reason,
            message: message.map(str::to_owned),
        };
        for (gate, status, expected) in [
            (
                wait(EnvironmentWaitReason::Provisioning),
                StatusCode::SERVICE_UNAVAILABLE,
                DataRouteRefusal::Provisioning {},
            ),
            (
                wait(EnvironmentWaitReason::Disconnected),
                StatusCode::SERVICE_UNAVAILABLE,
                DataRouteRefusal::Disconnected {},
            ),
            (
                gone(EnvironmentGoneReason::Closing, None),
                StatusCode::GONE,
                DataRouteRefusal::Gone {},
            ),
            (
                gone(EnvironmentGoneReason::Closed, None),
                StatusCode::GONE,
                DataRouteRefusal::Gone {},
            ),
            (
                gone(EnvironmentGoneReason::Away, Some("daemon away since 1")),
                StatusCode::GONE,
                DataRouteRefusal::Away {},
            ),
            (
                gone(EnvironmentGoneReason::Failed, Some("boot disk missing")),
                StatusCode::CONFLICT,
                DataRouteRefusal::Failed {
                    message: DescriptiveText::new("boot disk missing").unwrap(),
                },
            ),
        ] {
            let response = gate_refusal(&gate).expect("the gate refuses");
            assert_eq!(response.status(), status, "{gate:?}");
            assert_eq!(body_of(response), expected, "{gate:?}");
        }
    }

    #[test]
    fn registration_origins_come_from_the_public_base_url_and_configured_urls() {
        let loopback: SocketAddr = "127.0.0.1:18080".parse().unwrap();
        let public: SocketAddr = "0.0.0.0:18080".parse().unwrap();
        let origins = |base: Option<&str>, urls: &[&str], bind| {
            registration_origins(
                base,
                &urls.iter().map(|url| (*url).to_owned()).collect::<Vec<_>>(),
                bind,
            )
            .map(|origins| {
                origins
                    .iter()
                    .map(|origin| origin.as_str().to_owned())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            origins(None, &[], loopback).unwrap(),
            ["ws://127.0.0.1:18080"]
        );
        assert!(
            origins(None, &[], public).is_err(),
            "a non-loopback bind needs a configured origin"
        );
        assert_eq!(
            origins(Some("https://Lightspeed.example/base/"), &[], public).unwrap(),
            ["wss://lightspeed.example"]
        );
        assert!(origins(Some("lightspeed.example"), &[], public).is_err());
        assert_eq!(
            origins(
                Some("https://lightspeed.example"),
                &[
                    " ws://lightspeed-gateway.lightspeed.svc:18080/environment-gateway/connect ",
                    "",
                    "https://lightspeed.example/other",
                ],
                public,
            )
            .unwrap(),
            [
                "wss://lightspeed.example",
                "ws://lightspeed-gateway.lightspeed.svc:18080"
            ],
            "configured URLs are reduced to origins, trimmed and deduplicated"
        );
        assert_eq!(
            origins(None, &["ws://gateway.internal:18080"], public).unwrap(),
            ["ws://gateway.internal:18080"],
            "a configured URL alone serves a non-loopback bind"
        );
        assert!(origins(None, &["gateway.internal"], public).is_err());
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
