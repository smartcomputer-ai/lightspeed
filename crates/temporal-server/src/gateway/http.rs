use std::{net::SocketAddr, sync::Arc, time::Duration};

use api::{
    AgentApiError, JsonRpcRequest, JsonRpcResponse, dispatch_json_rpc, dispatch_operator_json_rpc,
    is_operator_method,
};
use auth::{ApiKeyStore, PrincipalKind, PrincipalRef, api_key_hash};
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
use futures_util::{SinkExt as _, StreamExt as _};
use host_protocol::gateway::{PROVIDER_DATA_PATH_PREFIX, ROUTE_PATH_PREFIX};
use serde::Deserialize;
use store_pg::{PgApiKeyStore, PgStore};
use temporalio_client::Client;
use tokio_tungstenite::{connect_async, tungstenite::Message as ProviderMessage};
use uuid::Uuid;

use crate::{
    config::{DeploymentStores, GatewayAuthMode, gateway_auth_mode_from_env},
    environment_gateway::{RouteKey, bearer_matches, close_message},
    universe::{UniverseError, UniverseRuntime},
};

use super::{
    GatewayAgentApi, GatewayOperatorApi, OAuthCallbackOutcome, connect_temporal, principal,
};

pub const DEFAULT_GATEWAY_BIND: &str = "127.0.0.1:18080";
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Trusted-header tenant selector. Only honored in `trusted-header` auth mode,
/// where an upstream gateway owns authentication and injects it; other modes
/// reject requests carrying it so tenant claims cannot be smuggled past a
/// different resolution mode.
pub const UNIVERSE_HEADER: &str = "x-lightspeed-universe";

/// Optional trusted-header caller identity, injected by the upstream gateway
/// alongside [`UNIVERSE_HEADER`]. Value is `<kind>:<id>` with kind `user` or
/// `service_account`, or a bare id (treated as a user id). Recorded on grants
/// and flows for audit; never an authorization mechanism.
pub const PRINCIPAL_HEADER: &str = "x-lightspeed-principal";

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
        operator: Arc<GatewayOperatorApi>,
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
        let api_keys = PgApiKeyStore::new(runtime.stores().pool().clone());
        let operator = Arc::new(GatewayOperatorApi::new(runtime.clone()));
        Self {
            resolution: UniverseResolution::Multi {
                mode,
                runtime,
                public_base_url,
                api_keys,
                operator,
            },
        }
    }

    /// Resolve the operator service for a request. Operator methods are
    /// deployment-addressed, so they exist only on deployment gateways —
    /// never fixed-instance ones.
    fn operator_for_request(
        &self,
        headers: &HeaderMap,
    ) -> Result<&Arc<GatewayOperatorApi>, AgentApiError> {
        match &self.resolution {
            UniverseResolution::FixedApi { .. } => Err(AgentApiError::rejected(
                "operator methods are not available on this gateway",
            )),
            UniverseResolution::Multi { mode, operator, .. } => {
                authorize_operator_call(mode, headers)?;
                Ok(operator)
            }
        }
    }

    async fn api_for_request(
        &self,
        headers: &HeaderMap,
    ) -> Result<(Arc<GatewayAgentApi>, PrincipalRef), AgentApiError> {
        match &self.resolution {
            UniverseResolution::FixedApi { api } => {
                reject_tenant_headers(headers)?;
                Ok((api.clone(), PrincipalRef::universe_default()))
            }
            UniverseResolution::Multi {
                mode,
                runtime,
                api_keys,
                ..
            } => {
                let (universe_id, create_missing, principal) = match mode {
                    GatewayAuthMode::Single { universe_id } => {
                        reject_tenant_headers(headers)?;
                        (*universe_id, true, PrincipalRef::universe_default())
                    }
                    // Never auto-creates: a typo'd universe header fails
                    // closed instead of silently materializing a tenant.
                    GatewayAuthMode::TrustedHeader => (
                        universe_from_header(headers)?,
                        false,
                        principal_from_header(headers)?,
                    ),
                    GatewayAuthMode::ApiKey => {
                        reject_tenant_headers(headers)?;
                        let record = resolve_api_key(api_keys, headers).await?;
                        (record.universe_id, false, record.principal)
                    }
                };
                let state = runtime
                    .state_for(universe_id, create_missing)
                    .await
                    .map_err(map_universe_error)?;
                Ok((state.api.clone(), principal))
            }
        }
    }

    async fn api_for_daemon(
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

/// Resolve `Authorization: Bearer lsk_…` against the deployment api_keys
/// table. Unknown and revoked keys are indistinguishable to the caller.
async fn resolve_api_key(
    api_keys: &PgApiKeyStore,
    headers: &HeaderMap,
) -> Result<auth::ApiKeyRecord, AgentApiError> {
    let secret = bearer_token(headers)?;
    let observed_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);
    api_keys
        .resolve_api_key(&api_key_hash(secret), observed_at_ms)
        .await
        .map_err(|error| AgentApiError::internal(format!("api key resolution failed: {error}")))?
        .ok_or_else(|| AgentApiError::rejected("invalid api key"))
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, AgentApiError> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or_else(|| AgentApiError::invalid_request("missing Authorization header"))?;
    let value = value
        .to_str()
        .map_err(|_| AgentApiError::invalid_request("invalid Authorization header encoding"))?;
    value
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            AgentApiError::invalid_request("Authorization header must be a Bearer token")
        })
}

/// Parse the optional trusted-header principal: `<kind>:<id>` with kind
/// `user` or `service_account`, or a bare id treated as a user id. Absent
/// header falls back to `universe_default`.
fn principal_from_header(headers: &HeaderMap) -> Result<PrincipalRef, AgentApiError> {
    let Some(value) = headers.get(PRINCIPAL_HEADER) else {
        return Ok(PrincipalRef::universe_default());
    };
    let value = value.to_str().map_err(|_| {
        AgentApiError::invalid_request(format!("invalid {PRINCIPAL_HEADER} header encoding"))
    })?;
    let value = value.trim();
    if value.is_empty() {
        return Ok(PrincipalRef::universe_default());
    }
    let (kind, id) = match value.split_once(':') {
        Some(("user", id)) => (PrincipalKind::User, id),
        Some(("service_account", id)) => (PrincipalKind::ServiceAccount, id),
        Some((other, _)) => {
            return Err(AgentApiError::invalid_request(format!(
                "invalid {PRINCIPAL_HEADER} kind {other:?}; expected user or service_account"
            )));
        }
        None => (PrincipalKind::User, value),
    };
    if id.is_empty() {
        return Err(AgentApiError::invalid_request(format!(
            "{PRINCIPAL_HEADER} id must not be empty"
        )));
    }
    Ok(PrincipalRef {
        kind,
        id: Some(id.to_owned()),
    })
}

/// Authorization boundary of the operator scope: operator methods are
/// callable by `trusted-header` and `single` callers only — an api-key
/// caller is a universe-bound tenant, not the platform. A universe header on
/// an operator call is a tenant claim that will not be honored and is
/// rejected (fail closed); the principal header stays allowed for
/// audit-stamping proxies.
fn authorize_operator_call(
    mode: &GatewayAuthMode,
    headers: &HeaderMap,
) -> Result<(), AgentApiError> {
    match mode {
        GatewayAuthMode::ApiKey => Err(AgentApiError::rejected(
            "operator methods are not available to api-key callers",
        )),
        GatewayAuthMode::Single { .. } | GatewayAuthMode::TrustedHeader => {
            if headers.contains_key(UNIVERSE_HEADER) {
                return Err(AgentApiError::invalid_request(format!(
                    "{UNIVERSE_HEADER} is not accepted on operator methods"
                )));
            }
            Ok(())
        }
    }
}

/// Modes that do not honor the trusted tenant headers must not silently
/// ignore them: a tenant claim that is not going to be honored is rejected.
fn reject_tenant_headers(headers: &HeaderMap) -> Result<(), AgentApiError> {
    for header in [UNIVERSE_HEADER, PRINCIPAL_HEADER] {
        if headers.contains_key(header) {
            return Err(AgentApiError::invalid_request(format!(
                "{header} is not accepted in this auth mode"
            )));
        }
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
    let app = gateway_router(state, config.max_request_body_bytes);
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(target: "temporal_server", bind = %config.bind, "gateway listening");
    axum::serve(listener, app).await?;
    reconciler.abort();
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
    let reconciler_api = api.clone();
    let reconciler = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            interval.tick().await;
            if let Err(error) = reconciler_api.reconcile_environment_lifecycle_once().await {
                tracing::warn!(target: "temporal_server", %error, "environment lifecycle reconcile pass failed");
            }
        }
    });
    let state = Arc::new(GatewayState::for_api(api));
    let app = gateway_router(state, config.max_request_body_bytes);
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(target: "temporal_server", bind = %config.bind, "gateway listening");
    axum::serve(listener, app).await?;
    reconciler.abort();
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
        .route(
            &format!("{ROUTE_PATH_PREFIX}/:universe/:environment/:incarnation"),
            get(environment_route_upgrade),
        )
        .layer(DefaultBodyLimit::max(max_request_body_bytes))
        .with_state(state)
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
            if connection.transport != host_protocol::shared::HostTransport::WebSocket {
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
    if assigned_connection.transport != host_protocol::shared::HostTransport::WebSocket
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
    // Operator methods branch before universe resolution: they address the
    // deployment and never resolve a universe.
    if is_operator_method(&request.method) {
        let operator = match state.operator_for_request(&headers) {
            Ok(operator) => operator,
            Err(error) => {
                return no_store_json_rpc(JsonRpcResponse::failure(request.id, error.into()));
            }
        };
        return no_store_json_rpc(dispatch_operator_json_rpc(operator.as_ref(), request).await);
    }
    let (api, caller) = match state.api_for_request(&headers).await {
        Ok(resolved) => resolved,
        Err(error) => {
            return no_store_json_rpc(JsonRpcResponse::failure(request.id, error.into()));
        }
    };
    no_store_json_rpc(
        principal::with_request_principal(caller, dispatch_json_rpc(api.as_ref(), request)).await,
    )
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
    fn universe_header_resolves_a_valid_uuid() {
        let universe_id = Uuid::parse_str("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f").expect("uuid");
        let headers = headers_with_universe(&universe_id.to_string());
        assert_eq!(
            universe_from_header(&headers).expect("resolve"),
            universe_id
        );
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
    fn non_header_modes_reject_tenant_header_smuggling() {
        // `single` and `api-key` modes (and any fixed-instance gateway) must
        // not silently ignore a tenant claim they do not honor.
        let headers = headers_with_universe("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f");
        let error = reject_tenant_headers(&headers).expect_err("must reject");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
        let mut principal_headers = HeaderMap::new();
        principal_headers.insert(PRINCIPAL_HEADER, "user:alice".parse().expect("header"));
        let error = reject_tenant_headers(&principal_headers).expect_err("must reject");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
        assert!(reject_tenant_headers(&HeaderMap::new()).is_ok());
    }

    #[test]
    fn principal_header_parses_kinds_and_defaults() {
        use auth::PrincipalKind;
        let parse = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(PRINCIPAL_HEADER, value.parse().expect("header"));
            principal_from_header(&headers)
        };
        assert_eq!(
            principal_from_header(&HeaderMap::new()).expect("absent header"),
            PrincipalRef::universe_default()
        );
        let user = parse("user:alice").expect("user principal");
        assert_eq!(user.kind, PrincipalKind::User);
        assert_eq!(user.id.as_deref(), Some("alice"));
        let service = parse("service_account:bridge-1").expect("service principal");
        assert_eq!(service.kind, PrincipalKind::ServiceAccount);
        assert_eq!(service.id.as_deref(), Some("bridge-1"));
        let bare = parse("alice").expect("bare principal");
        assert_eq!(bare.kind, PrincipalKind::User);
        assert_eq!(bare.id.as_deref(), Some("alice"));
        let error = parse("robot:r2d2").expect_err("unknown kind");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
        let error = parse("user:").expect_err("empty id");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
    }

    #[test]
    fn bearer_token_requires_a_nonempty_bearer_value() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer lsk_secret".parse().expect("header"),
        );
        assert_eq!(bearer_token(&headers).expect("token"), "lsk_secret");
        let error = bearer_token(&HeaderMap::new()).expect_err("missing header");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
        let mut basic = HeaderMap::new();
        basic.insert(
            axum::http::header::AUTHORIZATION,
            "Basic dXNlcg==".parse().expect("header"),
        );
        let error = bearer_token(&basic).expect_err("non-bearer");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);
    }

    #[test]
    fn unknown_universe_maps_to_not_found() {
        let error = map_universe_error(UniverseError::Unknown {
            universe_id: Uuid::nil(),
        });
        assert_eq!(error.kind, AgentApiErrorKind::NotFound);
    }

    #[test]
    fn operator_calls_are_gated_by_auth_mode_and_reject_universe_claims() {
        let single = GatewayAuthMode::Single {
            universe_id: Uuid::nil(),
        };
        assert!(authorize_operator_call(&single, &HeaderMap::new()).is_ok());
        assert!(
            authorize_operator_call(&GatewayAuthMode::TrustedHeader, &HeaderMap::new()).is_ok()
        );

        let error = authorize_operator_call(&GatewayAuthMode::ApiKey, &HeaderMap::new())
            .expect_err("api-key callers are universe-bound tenants, not the platform");
        assert_eq!(error.kind, AgentApiErrorKind::Rejected);

        // A universe claim on a deployment-addressed call fails closed.
        let headers = headers_with_universe("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f");
        let error = authorize_operator_call(&GatewayAuthMode::TrustedHeader, &headers)
            .expect_err("universe header must be rejected on operator calls");
        assert_eq!(error.kind, AgentApiErrorKind::InvalidRequest);

        // The principal header stays allowed for audit-stamping proxies.
        let mut principal_headers = HeaderMap::new();
        principal_headers.insert(PRINCIPAL_HEADER, "user:admin".parse().expect("header"));
        assert!(
            authorize_operator_call(&GatewayAuthMode::TrustedHeader, &principal_headers).is_ok()
        );
    }
}
