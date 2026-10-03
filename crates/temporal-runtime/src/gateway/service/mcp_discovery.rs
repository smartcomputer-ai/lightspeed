use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use auth::{PinnedHttpPolicy, SecretValue};
use futures_util::{StreamExt, stream::BoxStream};
use http::{HeaderName, HeaderValue};
use mcp::{
    DiscoveredMcpTool, McpToolAnnotations, McpToolDiscoveryFailure,
    McpToolDiscoveryFailureKind as FailureKind, McpToolDiscoveryLimits,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientInfo, ClientJsonRpcMessage, ClientRequest,
    Implementation, PaginatedRequestParams, ProtocolVersion, ServerJsonRpcMessage, Tool,
};
use rmcp::transport::{
    StreamableHttpClientTransport,
    streamable_http_client::{
        AuthRequiredError, InsufficientScopeError, SseError, StreamableHttpClient,
        StreamableHttpClientTransportConfig, StreamableHttpError, StreamableHttpPostResponse,
    },
};
use rmcp::{
    ClientLifecycleMode, ClientServiceExt, RoleClient,
    service::{ClientInitializeError, RunningService},
};
use serde_json::Value;
use sse_stream::{Sse, SseStream};
use url::Url;

const JSON_MIME_TYPE: &str = "application/json";
const EVENT_STREAM_MIME_TYPE: &str = "text/event-stream";
const HEADER_SESSION_ID: &str = "mcp-session-id";
const HEADER_LAST_EVENT_ID: &str = "last-event-id";

pub(super) struct McpDiscoveryGate {
    state: Arc<Mutex<McpDiscoveryGateState>>,
    cooldown: Duration,
}

#[derive(Default)]
struct McpDiscoveryGateState {
    active: BTreeSet<String>,
    completed: BTreeMap<String, std::time::Instant>,
}

pub(super) struct McpDiscoveryPermit {
    state: Arc<Mutex<McpDiscoveryGateState>>,
    key: String,
}

impl McpDiscoveryGate {
    pub(super) fn new(cooldown: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(McpDiscoveryGateState::default())),
            cooldown,
        }
    }

    pub(super) fn try_start(&self, key: &str) -> Result<McpDiscoveryPermit, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "admission state unavailable")?;
        if state.active.contains(key) {
            return Err("tool discovery is already running for this MCP server");
        }
        if state
            .completed
            .get(key)
            .is_some_and(|completed| completed.elapsed() < self.cooldown)
        {
            return Err("tool discovery was just completed for this MCP server; retry shortly");
        }
        state.active.insert(key.to_owned());
        Ok(McpDiscoveryPermit {
            state: self.state.clone(),
            key: key.to_owned(),
        })
    }
}

impl Drop for McpDiscoveryPermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.active.remove(&self.key);
            state
                .completed
                .insert(self.key.clone(), std::time::Instant::now());
        }
    }
}

#[async_trait]
pub(crate) trait McpToolDiscoverer: Send + Sync {
    async fn discover_tools(
        &self,
        server_url: &str,
        bearer: Option<&SecretValue>,
        trusted_universe: Option<uuid::Uuid>,
        allow_private_network: bool,
        limits: McpToolDiscoveryLimits,
    ) -> Result<DiscoveredMcpInventory, McpToolDiscoveryFailure>;
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DiscoveredMcpInventory {
    pub tools: Vec<DiscoveredMcpTool>,
    pub tools_list_changed: bool,
    pub ttl_ms: Option<u64>,
}

#[derive(Clone, Debug)]
pub(crate) struct HttpMcpToolDiscoverer {
    public_http_policy: PinnedHttpPolicy,
    private_http_policy: PinnedHttpPolicy,
    timeout: Duration,
}

pub(crate) struct McpToolCallRequest {
    pub tool_name: String,
    pub arguments: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ConfiguratorTrustedHeaderPolicy;

impl ConfiguratorTrustedHeaderPolicy {
    pub(crate) fn from_env() -> Result<Self, String> {
        Self::parse(
            std::env::var("LIGHTSPEED_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER_URL")
                .ok()
                .as_deref(),
        )
    }
    fn parse(value: Option<&str>) -> Result<Self, String> {
        if value.is_some_and(|v| !v.trim().is_empty()) {
            return Err("Configurator trusted-header authentication is retired; install a scoped service key".into());
        }
        Ok(Self)
    }
    pub(crate) fn permits(&self, _server_id: &str, _server_url: &str) -> bool {
        false
    }
}

#[derive(Clone)]
struct BoundedReqwestClient {
    client: reqwest::Client,
    budget: ResponseBudget,
    diagnostics: TransportDiagnostics,
    operation: ResponseOperation,
}

#[derive(Clone, Copy)]
enum ResponseOperation {
    Discovery,
    ToolCall,
}

impl ResponseOperation {
    fn label(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::ToolCall => "tool call",
        }
    }
}

#[derive(Clone, Default)]
struct TransportDiagnostics {
    failure: Arc<Mutex<Option<McpToolDiscoveryFailure>>>,
    legacy_bootstrap_rejected: Arc<AtomicBool>,
}

impl TransportDiagnostics {
    fn record(&self, failure: McpToolDiscoveryFailure) {
        if let Ok(mut current) = self.failure.lock()
            && current.is_none()
        {
            *current = Some(failure);
        }
    }

    fn current(&self) -> Option<McpToolDiscoveryFailure> {
        self.failure.lock().ok().and_then(|failure| failure.clone())
    }

    fn record_legacy_bootstrap_rejection(&self) {
        self.legacy_bootstrap_rejected
            .store(true, Ordering::Relaxed);
    }

    fn legacy_bootstrap_was_rejected(&self) -> bool {
        self.legacy_bootstrap_rejected.load(Ordering::Relaxed)
    }
}

#[derive(Clone)]
struct ResponseBudget {
    consumed: Arc<AtomicUsize>,
    max: usize,
}

impl ResponseBudget {
    fn new(max: usize) -> Self {
        Self {
            consumed: Arc::new(AtomicUsize::new(0)),
            max,
        }
    }

    fn reserve(&self, bytes: usize) -> Result<(), ResponseLimitError> {
        self.consumed
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |consumed| {
                consumed
                    .checked_add(bytes)
                    .filter(|total| *total <= self.max)
            })
            .map(|_| ())
            .map_err(|_| ResponseLimitError)
    }

    fn can_fit(&self, bytes: u64) -> bool {
        usize::try_from(bytes).is_ok_and(|bytes| {
            self.consumed
                .load(Ordering::Relaxed)
                .checked_add(bytes)
                .is_some_and(|total| total <= self.max)
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("MCP response exceeded its byte limit")]
struct ResponseLimitError;

#[derive(Debug, thiserror::Error)]
enum BoundedStreamError {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Limit(#[from] ResponseLimitError),
}

impl BoundedReqwestClient {
    fn new(
        client: reqwest::Client,
        max_response_bytes: usize,
        operation: ResponseOperation,
    ) -> Self {
        Self {
            client,
            budget: ResponseBudget::new(max_response_bytes),
            diagnostics: TransportDiagnostics::default(),
            operation,
        }
    }

    fn response_too_large(&self) -> StreamableHttpError<reqwest::Error> {
        self.diagnostics.record(failure(
            FailureKind::ResponseTooLarge,
            format!(
                "MCP {} exceeded the total response byte limit",
                self.operation.label()
            ),
        ));
        StreamableHttpError::UnexpectedServerResponse(Cow::Owned(format!(
            "MCP response exceeded the {} byte limit",
            self.operation.label()
        )))
    }

    fn apply_headers(
        mut request: reqwest::RequestBuilder,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> reqwest::RequestBuilder {
        for (name, value) in headers {
            request = request.header(name, value);
        }
        request
    }

    fn validate_content_length(
        &self,
        response: &reqwest::Response,
    ) -> Result<(), StreamableHttpError<reqwest::Error>> {
        if response
            .content_length()
            .is_some_and(|length| !self.budget.can_fit(length))
        {
            return Err(self.response_too_large());
        }
        Ok(())
    }

    async fn read_body(
        &self,
        response: reqwest::Response,
    ) -> Result<Vec<u8>, StreamableHttpError<reqwest::Error>> {
        self.validate_content_length(&response)?;
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(StreamableHttpError::Client)?;
            self.budget
                .reserve(chunk.len())
                .map_err(|_| self.response_too_large())?;
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    fn sse_stream(
        &self,
        response: reqwest::Response,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<reqwest::Error>>
    {
        self.validate_content_length(&response)?;
        let budget = self.budget.clone();
        let diagnostics = self.diagnostics.clone();
        let operation = self.operation;
        let chunks = response.bytes_stream().map(move |chunk| match chunk {
            Ok(chunk) => match budget.reserve(chunk.len()) {
                Ok(()) => Ok(chunk),
                Err(error) => {
                    diagnostics.record(failure(
                        FailureKind::ResponseTooLarge,
                        format!(
                            "MCP {} exceeded the total response byte limit",
                            operation.label()
                        ),
                    ));
                    Err(BoundedStreamError::from(error))
                }
            },
            Err(error) => {
                diagnostics.record(request_failure(&error, operation));
                Err(BoundedStreamError::from(error))
            }
        });
        Ok(SseStream::from_bytes_stream(chunks).boxed())
    }

    fn authenticated(
        mut request: reqwest::RequestBuilder,
        token: Option<String>,
    ) -> reqwest::RequestBuilder {
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request
    }

    fn auth_failure(
        &self,
        response: &reqwest::Response,
    ) -> Option<StreamableHttpError<reqwest::Error>> {
        let challenge = response
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        if let Ok(parsed) = auth::parse_mcp_oauth_challenge(&challenge, response.url().as_str()) {
            if parsed.insufficient_scope && !parsed.required_scopes.is_empty() {
                self.diagnostics.record(
                    failure(
                        FailureKind::AdditionalConsentRequired,
                        "MCP endpoint requires additional OAuth consent",
                    )
                    .with_required_scopes(parsed.required_scopes),
                );
            } else if parsed.invalid_token {
                self.diagnostics.record(failure(
                    FailureKind::GrantNeedsReauth,
                    "MCP endpoint rejected the OAuth token; reconnect this server",
                ));
            }
        }
        match response.status() {
            reqwest::StatusCode::UNAUTHORIZED => Some(StreamableHttpError::AuthRequired(
                AuthRequiredError::new(challenge),
            )),
            reqwest::StatusCode::FORBIDDEN => Some(StreamableHttpError::InsufficientScope(
                InsufficientScopeError::new(challenge, None),
            )),
            _ => None,
        }
    }
}

impl StreamableHttpClient for BoundedReqwestClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let mut request = self
            .client
            .post(uri.as_ref())
            .header(
                reqwest::header::ACCEPT,
                format!("{EVENT_STREAM_MIME_TYPE}, {JSON_MIME_TYPE}"),
            )
            .json(&message);
        request = Self::authenticated(request, auth_header);
        request = Self::apply_headers(request, custom_headers);
        let session_was_attached = session_id.is_some();
        let is_sessionless_discover = !session_was_attached
            && matches!(
                &message,
                ClientJsonRpcMessage::Request(request)
                    if matches!(&request.request, ClientRequest::DiscoverRequest(_))
            );
        if let Some(session_id) = session_id {
            request = request.header(HEADER_SESSION_ID, session_id.as_ref());
        }
        let response = request.send().await.map_err(|error| {
            self.diagnostics
                .record(request_failure(&error, self.operation));
            StreamableHttpError::Client(error)
        })?;
        if let Some(error) = self.auth_failure(&response) {
            self.diagnostics
                .record(status_failure(response.status(), self.operation));
            return Err(error);
        }
        let status = response.status();
        if matches!(
            status,
            reqwest::StatusCode::ACCEPTED | reqwest::StatusCode::NO_CONTENT
        ) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status == reqwest::StatusCode::NOT_FOUND && session_was_attached {
            return Err(StreamableHttpError::SessionExpired);
        }
        if !status.is_success() {
            if is_sessionless_discover && is_legacy_bootstrap_rejection(status) {
                self.diagnostics.record_legacy_bootstrap_rejection();
            }
            self.diagnostics
                .record(status_failure(status, self.operation));
            return Err(StreamableHttpError::Client(
                response.error_for_status().expect_err("non-success status"),
            ));
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string());
        let response_session_id = response
            .headers()
            .get(HEADER_SESSION_ID)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        match content_type.as_deref() {
            Some(value) if value.starts_with(EVENT_STREAM_MIME_TYPE) => Ok(
                StreamableHttpPostResponse::Sse(self.sse_stream(response)?, response_session_id),
            ),
            Some(value) if value.starts_with(JSON_MIME_TYPE) => {
                let body = self.read_body(response).await?;
                if body.is_empty()
                    && matches!(
                        message,
                        ClientJsonRpcMessage::Notification(_)
                            | ClientJsonRpcMessage::Response(_)
                            | ClientJsonRpcMessage::Error(_)
                    )
                {
                    return Ok(StreamableHttpPostResponse::Accepted);
                }
                let message =
                    serde_json::from_slice::<ServerJsonRpcMessage>(&body).map_err(|error| {
                        self.diagnostics.record(failure(
                            FailureKind::InvalidResponse,
                            "MCP endpoint returned invalid JSON",
                        ));
                        StreamableHttpError::Deserialize(error)
                    })?;
                Ok(StreamableHttpPostResponse::Json(
                    message,
                    response_session_id,
                ))
            }
            _ => {
                self.diagnostics.record(failure(
                    FailureKind::InvalidResponse,
                    "MCP endpoint returned an invalid Streamable HTTP content type",
                ));
                Err(StreamableHttpError::UnexpectedContentType(content_type))
            }
        }
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        let request = self
            .client
            .delete(uri.as_ref())
            .header(HEADER_SESSION_ID, session_id.as_ref());
        let request = Self::authenticated(request, auth_header);
        let request = Self::apply_headers(request, custom_headers);
        let response = request.send().await.map_err(StreamableHttpError::Client)?;
        if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            return Ok(());
        }
        response
            .error_for_status()
            .map(|_| ())
            .map_err(StreamableHttpError::Client)
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        let mut request = self
            .client
            .get(uri.as_ref())
            .header(reqwest::header::ACCEPT, EVENT_STREAM_MIME_TYPE);
        if let Some(session_id) = session_id {
            request = request.header(HEADER_SESSION_ID, session_id.as_ref());
        }
        if let Some(last_event_id) = last_event_id {
            request = request.header(HEADER_LAST_EVENT_ID, last_event_id);
        }
        request = Self::authenticated(request, auth_header);
        request = Self::apply_headers(request, custom_headers);
        let response = request.send().await.map_err(StreamableHttpError::Client)?;
        if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        if let Some(error) = self.auth_failure(&response) {
            return Err(error);
        }
        let response = response
            .error_for_status()
            .map_err(StreamableHttpError::Client)?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string());
        if !content_type
            .as_deref()
            .is_some_and(|value| value.starts_with(EVENT_STREAM_MIME_TYPE))
        {
            return Err(StreamableHttpError::UnexpectedContentType(content_type));
        }
        self.sse_stream(response)
    }
}

impl HttpMcpToolDiscoverer {
    pub(crate) fn new() -> Self {
        // One deadline covers DNS, initialization, every tools/list page, and
        // shutdown. Large remote catalogs need more than the tiny-fixture
        // budget while still remaining strictly bounded.
        let timeout = Duration::from_secs(30);
        Self {
            public_http_policy: PinnedHttpPolicy::public_only().with_timeout(timeout),
            private_http_policy: PinnedHttpPolicy::allowing_private_networks()
                .with_timeout(timeout),
            timeout,
        }
    }

    async fn pinned_client(
        &self,
        endpoint: &Url,
        allow_private_network: bool,
    ) -> Result<reqwest::Client, McpToolDiscoveryFailure> {
        let policy = if allow_private_network {
            &self.private_http_policy
        } else {
            &self.public_http_policy
        };
        policy.client_for_url(endpoint).await.map_err(|error| {
            failure(
                FailureKind::Unreachable,
                format!("MCP endpoint rejected by outbound network policy: {error}"),
            )
        })
    }

    async fn start_service(
        &self,
        client: &reqwest::Client,
        endpoint: &Url,
        bearer: Option<&SecretValue>,
        trusted_universe: Option<uuid::Uuid>,
        max_response_bytes: usize,
        operation: ResponseOperation,
    ) -> Result<(McpClientService, TransportDiagnostics), McpToolDiscoveryFailure> {
        let first = start_service_once(
            client,
            endpoint,
            bearer,
            trusted_universe,
            max_response_bytes,
            operation,
            client_lifecycle(),
        )
        .await;
        match first {
            Ok(started) => Ok(started),
            Err(error) if error.retry_legacy => {
                tracing::info!(
                    operation = operation.label(),
                    "MCP server rejected sessionless discovery bootstrap; retrying legacy initialize"
                );
                start_service_once(
                    client,
                    endpoint,
                    bearer,
                    trusted_universe,
                    max_response_bytes,
                    operation,
                    ClientLifecycleMode::Initialize,
                )
                .await
                .map_err(|error| error.failure)
            }
            Err(error) => Err(error.failure),
        }
    }

    pub(crate) async fn call_tool(
        &self,
        server_url: &str,
        bearer: Option<&SecretValue>,
        trusted_universe: Option<uuid::Uuid>,
        allow_private_network: bool,
        limits: McpToolDiscoveryLimits,
        request: McpToolCallRequest,
    ) -> Result<CallToolResult, McpToolDiscoveryFailure> {
        let endpoint = Url::parse(server_url)
            .map_err(|_| failure(FailureKind::InvalidResponse, "MCP endpoint URL is invalid"))?;
        if endpoint.username() != ""
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(failure(
                FailureKind::InvalidResponse,
                "MCP endpoint URL must not contain credentials or a fragment",
            ));
        }
        tokio::time::timeout(self.timeout, async {
            let client = self.pinned_client(&endpoint, allow_private_network).await?;
            let (service, diagnostics) = self
                .start_service(
                    &client,
                    &endpoint,
                    bearer,
                    trusted_universe,
                    limits.max_tool_call_response_bytes,
                    ResponseOperation::ToolCall,
                )
                .await?;
            let result = service
                .call_tool(
                    CallToolRequestParams::new(request.tool_name).with_arguments(request.arguments),
                )
                .await
                .map_err(|error| {
                    diagnostics
                        .current()
                        .unwrap_or_else(|| map_service_error(error, ResponseOperation::ToolCall))
                });
            let _ = service.cancel().await;
            result
        })
        .await
        .map_err(|_| failure(FailureKind::Unreachable, "MCP tool call timed out"))?
    }
}

#[async_trait]
impl McpToolDiscoverer for HttpMcpToolDiscoverer {
    async fn discover_tools(
        &self,
        server_url: &str,
        bearer: Option<&SecretValue>,
        trusted_universe: Option<uuid::Uuid>,
        allow_private_network: bool,
        limits: McpToolDiscoveryLimits,
    ) -> Result<DiscoveredMcpInventory, McpToolDiscoveryFailure> {
        let endpoint = Url::parse(server_url)
            .map_err(|_| failure(FailureKind::InvalidResponse, "MCP endpoint URL is invalid"))?;
        if endpoint.username() != ""
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(failure(
                FailureKind::InvalidResponse,
                "MCP endpoint URL must not contain credentials or a fragment",
            ));
        }

        tokio::time::timeout(self.timeout, async {
            let client = self.pinned_client(&endpoint, allow_private_network).await?;
            let (service, diagnostics) = self
                .start_service(
                    &client,
                    &endpoint,
                    bearer,
                    trusted_universe,
                    limits.max_response_bytes,
                    ResponseOperation::Discovery,
                )
                .await?;

            let tools_list_changed = service.peer_info().is_some_and(|info| {
                info.capabilities
                    .tools
                    .as_ref()
                    .is_some_and(|tools| tools.list_changed == Some(true))
            });
            let result =
                discover_pages(&service, limits, &diagnostics)
                    .await
                    .map(|(tools, ttl_ms)| DiscoveredMcpInventory {
                        tools,
                        tools_list_changed,
                        ttl_ms,
                    });
            let _ = service.cancel().await;
            result
        })
        .await
        .map_err(|_| failure(FailureKind::Unreachable, "MCP endpoint timed out"))?
    }
}

type McpClientService = RunningService<RoleClient, ClientInfo>;

struct StartServiceFailure {
    failure: McpToolDiscoveryFailure,
    retry_legacy: bool,
}

async fn start_service_once(
    client: &reqwest::Client,
    endpoint: &Url,
    bearer: Option<&SecretValue>,
    trusted_universe: Option<uuid::Uuid>,
    max_response_bytes: usize,
    operation: ResponseOperation,
    lifecycle: ClientLifecycleMode,
) -> Result<(McpClientService, TransportDiagnostics), StartServiceFailure> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(endpoint.as_str())
        .max_sse_event_size(max_response_bytes)
        .reinit_on_expired_session(false);
    config = transport_auth(config, bearer, trusted_universe).map_err(|failure| {
        StartServiceFailure {
            failure,
            retry_legacy: false,
        }
    })?;
    let client = BoundedReqwestClient::new(client.clone(), max_response_bytes, operation);
    let diagnostics = client.diagnostics.clone();
    let transport = StreamableHttpClientTransport::with_client(client, config);
    let result = ClientInfo::new(
        Default::default(),
        Implementation::new("lightspeed", env!("CARGO_PKG_VERSION")),
    )
    .serve_with_lifecycle(transport, lifecycle)
    .await;
    match result {
        Ok(service) => Ok((service, diagnostics)),
        Err(error) => Err(StartServiceFailure {
            retry_legacy: diagnostics.legacy_bootstrap_was_rejected(),
            failure: diagnostics
                .current()
                .unwrap_or_else(|| map_initialize_error(error, operation)),
        }),
    }
}

fn transport_auth(
    mut config: StreamableHttpClientTransportConfig,
    bearer: Option<&SecretValue>,
    trusted_universe: Option<uuid::Uuid>,
) -> Result<StreamableHttpClientTransportConfig, McpToolDiscoveryFailure> {
    if bearer.is_some() && trusted_universe.is_some() {
        return Err(failure(
            FailureKind::InvalidResponse,
            "MCP transport cannot combine bearer and trusted-header authentication",
        ));
    }
    if let Some(token) = bearer {
        config = config.auth_header(token.expose());
    }
    if let Some(universe_id) = trusted_universe {
        let mut headers = HashMap::new();
        headers.insert(
            HeaderName::from_static("x-lightspeed-universe"),
            HeaderValue::from_str(&universe_id.to_string()).expect("UUID is a valid header value"),
        );
        config = config.custom_headers(headers);
    }
    Ok(config)
}

fn client_lifecycle() -> ClientLifecycleMode {
    ClientLifecycleMode::Auto {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        legacy_version: Some(ProtocolVersion::V_2025_11_25),
    }
}

async fn discover_pages(
    service: &rmcp::service::RunningService<rmcp::RoleClient, ClientInfo>,
    limits: McpToolDiscoveryLimits,
    diagnostics: &TransportDiagnostics,
) -> Result<(Vec<DiscoveredMcpTool>, Option<u64>), McpToolDiscoveryFailure> {
    let mut tools = Vec::new();
    let mut seen_tools = BTreeSet::new();
    let mut seen_cursors = BTreeSet::new();
    let mut cursor = None;
    // The assembled inventory is fresh only as long as its least-fresh page.
    // If any legacy page omits ttlMs, use the resolver's capability fallback.
    let mut ttl_ms = Some(u64::MAX);
    for _ in 0..limits.max_pages {
        let response = service
            .list_tools(Some(
                PaginatedRequestParams::default().with_cursor(cursor.clone()),
            ))
            .await
            .map_err(|error| {
                diagnostics
                    .current()
                    .unwrap_or_else(|| map_service_error(error, ResponseOperation::Discovery))
            })?;
        ttl_ms = match (ttl_ms, response.ttl_ms) {
            (Some(current), Some(page)) => Some(current.min(page)),
            _ => None,
        };
        append_tools(response.tools, &mut tools, &mut seen_tools, limits)?;

        let Some(next_cursor) = response.next_cursor else {
            return Ok((tools, ttl_ms));
        };
        if next_cursor.is_empty() || next_cursor.len() > limits.max_text_bytes {
            return Err(failure(
                FailureKind::InvalidResponse,
                "MCP tools/list returned an invalid pagination cursor",
            ));
        }
        if !seen_cursors.insert(next_cursor.clone()) {
            return Err(failure(
                FailureKind::InvalidResponse,
                "MCP tools/list repeated a pagination cursor",
            ));
        }
        cursor = Some(next_cursor);
    }

    Err(failure(
        FailureKind::PaginationLimit,
        "MCP tools/list exceeded the discovery page limit",
    ))
}

fn append_tools(
    advertised: Vec<Tool>,
    tools: &mut Vec<DiscoveredMcpTool>,
    seen: &mut BTreeSet<String>,
    limits: McpToolDiscoveryLimits,
) -> Result<(), McpToolDiscoveryFailure> {
    if tools.len().saturating_add(advertised.len()) > limits.max_tools {
        return Err(failure(
            FailureKind::PaginationLimit,
            "MCP tools/list exceeded the discovery tool limit",
        ));
    }
    for tool in advertised {
        let tool = project_tool(tool, limits)?;
        if !seen.insert(tool.name.clone()) {
            return Err(failure(
                FailureKind::InvalidResponse,
                "MCP tools/list returned a duplicate tool name",
            ));
        }
        tools.push(tool);
    }
    Ok(())
}

fn project_tool(
    tool: Tool,
    limits: McpToolDiscoveryLimits,
) -> Result<DiscoveredMcpTool, McpToolDiscoveryFailure> {
    let name = bounded_required_text(tool.name.as_ref(), "tool name", limits.max_name_bytes)?;
    let direct_title = truncated_optional_text(tool.title, limits.max_text_bytes);
    let description = truncated_optional_text(
        tool.description.map(|value| value.into_owned()),
        limits.max_text_bytes,
    );

    let schema = Value::Object(tool.input_schema.as_ref().clone());
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(failure(
            FailureKind::InvalidResponse,
            "MCP tool inputSchema must have object as its root type",
        ));
    }
    let schema_bytes = serde_json::to_vec(&schema).map_err(|_| {
        failure(
            FailureKind::InvalidResponse,
            "MCP tool inputSchema is invalid",
        )
    })?;
    if schema_bytes.len() > limits.max_schema_bytes || json_depth(&schema) > limits.max_schema_depth
    {
        return Err(failure(
            FailureKind::ResponseTooLarge,
            "MCP tool inputSchema exceeded discovery limits",
        ));
    }

    let (annotation_title, annotations) = match tool.annotations {
        Some(annotations) => {
            let title = truncated_optional_text(annotations.title, limits.max_text_bytes);
            (
                title,
                Some(McpToolAnnotations {
                    read_only_hint: annotations.read_only_hint,
                    destructive_hint: annotations.destructive_hint,
                    idempotent_hint: annotations.idempotent_hint,
                    open_world_hint: annotations.open_world_hint,
                }),
            )
        }
        None => (None, None),
    };

    Ok(DiscoveredMcpTool {
        name,
        title: direct_title.or(annotation_title),
        description,
        input_schema: schema,
        annotations,
    })
}

fn bounded_required_text(
    value: &str,
    field: &str,
    max_bytes: usize,
) -> Result<String, McpToolDiscoveryFailure> {
    if value.is_empty() {
        return Err(failure(
            FailureKind::InvalidResponse,
            format!("MCP {field} is empty"),
        ));
    }
    if value.len() > max_bytes {
        return Err(failure(
            FailureKind::ResponseTooLarge,
            format!("MCP {field} exceeded the discovery limit"),
        ));
    }
    Ok(value.to_owned())
}

fn truncated_optional_text(value: Option<String>, max_bytes: usize) -> Option<String> {
    // Titles and descriptions are untrusted hints, not protocol identity or
    // execution authority. A verbose hint must not make every otherwise valid
    // tool on the server unusable. Names and schemas remain fail-closed; hints
    // degrade independently and visibly with an ellipsis.
    value.map(|value| mcp::truncate_utf8_with_ellipsis(value, max_bytes))
}

fn json_depth(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1 + values.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(values) => 1 + values.values().map(json_depth).max().unwrap_or(0),
        _ => 1,
    }
}

fn map_initialize_error(
    error: ClientInitializeError,
    operation: ResponseOperation,
) -> McpToolDiscoveryFailure {
    if matches!(
        error,
        ClientInitializeError::NoCompatibleProtocolVersion { .. }
            | ClientInitializeError::NoPreferredProtocolVersion
    ) {
        return failure(
            FailureKind::UnsupportedProtocol,
            "MCP endpoint does not support a compatible protocol version",
        );
    }
    failure_from_error_chain(&error, operation, "MCP endpoint initialization failed")
}

fn map_service_error(
    error: rmcp::service::ServiceError,
    operation: ResponseOperation,
) -> McpToolDiscoveryFailure {
    failure_from_error_chain(
        &error,
        operation,
        match operation {
            ResponseOperation::Discovery => "MCP tools/list failed",
            ResponseOperation::ToolCall => "MCP tools/call failed",
        },
    )
}

fn failure_from_error_chain(
    error: &(dyn std::error::Error + 'static),
    operation: ResponseOperation,
    fallback: &'static str,
) -> McpToolDiscoveryFailure {
    let mut current = Some(error);
    while let Some(source) = current {
        if source.is::<ResponseLimitError>() {
            return failure(
                FailureKind::ResponseTooLarge,
                format!(
                    "MCP {} exceeded the total response byte limit",
                    operation.label()
                ),
            );
        }
        if source.is::<AuthRequiredError>() {
            return failure(
                FailureKind::Unauthorized,
                "MCP endpoint rejected the credential",
            );
        }
        if source.is::<InsufficientScopeError>() {
            return failure(
                FailureKind::Forbidden,
                format!("MCP endpoint denied access to the {}", operation.label()),
            );
        }
        if let Some(error) = source.downcast_ref::<reqwest::Error>() {
            if let Some(status) = error.status() {
                return status_failure(status, operation);
            }
            return failure(
                FailureKind::Unreachable,
                if error.is_timeout() {
                    "MCP endpoint timed out"
                } else {
                    "MCP endpoint connection failed"
                },
            );
        }
        if let Some(error) = source.downcast_ref::<StreamableHttpError<reqwest::Error>>() {
            return match error {
                StreamableHttpError::UnexpectedServerResponse(message)
                    if message.contains("byte limit") =>
                {
                    failure(
                        FailureKind::ResponseTooLarge,
                        format!(
                            "MCP {} exceeded the total response byte limit",
                            operation.label()
                        ),
                    )
                }
                StreamableHttpError::UnexpectedContentType(_)
                | StreamableHttpError::UnexpectedServerResponse(_)
                | StreamableHttpError::Deserialize(_)
                | StreamableHttpError::UnexpectedEndOfStream => failure(
                    FailureKind::InvalidResponse,
                    "MCP endpoint returned an invalid Streamable HTTP response",
                ),
                StreamableHttpError::ServerDoesNotSupportSse
                | StreamableHttpError::ServerDoesNotSupportDeleteSession => {
                    failure(FailureKind::UnsupportedProtocol, fallback)
                }
                _ => {
                    current = source.source();
                    continue;
                }
            };
        }
        current = source.source();
    }
    failure(FailureKind::RemoteFailure, fallback)
}

fn status_failure(
    status: reqwest::StatusCode,
    operation: ResponseOperation,
) -> McpToolDiscoveryFailure {
    match status {
        reqwest::StatusCode::UNAUTHORIZED => failure(
            FailureKind::Unauthorized,
            "MCP endpoint rejected the credential",
        ),
        reqwest::StatusCode::FORBIDDEN => failure(
            FailureKind::Forbidden,
            format!("MCP endpoint denied access to the {}", operation.label()),
        ),
        reqwest::StatusCode::TOO_MANY_REQUESTS => failure(
            FailureKind::RemoteRateLimited,
            format!("MCP endpoint rate limited the {}", operation.label()),
        ),
        _ => failure(
            FailureKind::RemoteFailure,
            format!("MCP endpoint request failed with HTTP {status}"),
        ),
    }
}

fn is_legacy_bootstrap_rejection(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::BAD_REQUEST
            | reqwest::StatusCode::NOT_FOUND
            | reqwest::StatusCode::METHOD_NOT_ALLOWED
            | reqwest::StatusCode::NOT_ACCEPTABLE
            | reqwest::StatusCode::CONFLICT
            | reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
            | reqwest::StatusCode::UNPROCESSABLE_ENTITY
            | reqwest::StatusCode::NOT_IMPLEMENTED
    )
}

fn request_failure(
    error: &reqwest::Error,
    operation: ResponseOperation,
) -> McpToolDiscoveryFailure {
    match error.status() {
        Some(status) => status_failure(status, operation),
        None => failure(
            FailureKind::Unreachable,
            if error.is_timeout() {
                "MCP endpoint timed out"
            } else {
                "MCP endpoint connection failed"
            },
        ),
    }
}

fn failure(kind: FailureKind, message: impl Into<String>) -> McpToolDiscoveryFailure {
    McpToolDiscoveryFailure::new(kind, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{
            HeaderMap, HeaderValue, StatusCode,
            header::{AUTHORIZATION, WWW_AUTHENTICATE},
        },
        response::{IntoResponse, Response},
        routing::post,
    };
    use serde_json::json;
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct TestMcpState {
        discover_count: usize,
        initialize_count: usize,
        initialized_count: usize,
        tool_call_count: usize,
        authorizations: Vec<String>,
        discover_http_status: Option<StatusCode>,
        oversized_inventory: bool,
        list_delay: Option<Duration>,
        oauth_challenge: Option<String>,
        tools_list_changed: bool,
    }

    async fn streamable_handler(
        State(shared_state): State<Arc<Mutex<TestMcpState>>>,
        headers: HeaderMap,
        Json(request): Json<Value>,
    ) -> Response {
        let mut state = shared_state.lock().await;
        state.authorizations.push(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned(),
        );
        if let Some(challenge) = state.oauth_challenge.clone() {
            let mut response = StatusCode::UNAUTHORIZED.into_response();
            response.headers_mut().insert(
                WWW_AUTHENTICATE,
                HeaderValue::from_str(&challenge).expect("valid OAuth challenge fixture"),
            );
            return response;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        match request.get("method").and_then(Value::as_str) {
            Some("server/discover") => {
                state.discover_count += 1;
                if let Some(status) = state.discover_http_status {
                    let mut response = (
                        status,
                        Json(json!({
                            "jsonrpc": "2.0",
                            "id": "server-error",
                            "error": {"code": -32600, "message": "Missing session ID"}
                        })),
                    )
                        .into_response();
                    response.headers_mut().insert(
                        "mcp-session-id",
                        HeaderValue::from_static("incorrect-probe-session"),
                    );
                    return response;
                }
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": "Method not found"}
                }))
                .into_response()
            }
            Some("initialize") => {
                state.initialize_count += 1;
                let mut response = Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {
                            "tools": {"listChanged": state.tools_list_changed}
                        },
                        "serverInfo": {"name": "search-fixture", "version": "1"}
                    }
                }))
                .into_response();
                response
                    .headers_mut()
                    .insert("mcp-session-id", HeaderValue::from_static("test-session"));
                response
            }
            Some("notifications/initialized") => {
                state.initialized_count += 1;
                StatusCode::ACCEPTED.into_response()
            }
            Some("tools/list") => {
                if let Some(delay) = state.list_delay {
                    drop(state);
                    tokio::time::sleep(delay).await;
                    state = shared_state.lock().await;
                }
                if state.oversized_inventory {
                    return Json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "tools": [{
                                "name": "oversized",
                                "description": "x".repeat(2048),
                                "inputSchema": {"type": "object"}
                            }]
                        }
                    }))
                    .into_response();
                }
                let cursor = request.pointer("/params/cursor").and_then(Value::as_str);
                let result = if cursor.is_none() {
                    json!({
                        "tools": [{
                            "name": "search",
                            "title": "Search",
                            "description": "Search the fixture",
                            "inputSchema": {"type": "object"},
                            "annotations": {"readOnlyHint": true}
                        }],
                        "nextCursor": "second",
                        "ttlMs": 120_000
                    })
                } else {
                    json!({
                        "tools": [{
                            "name": "update",
                            "inputSchema": {"type": "object"},
                            "annotations": {"destructiveHint": true}
                        }],
                        "ttlMs": 60_000
                    })
                };
                Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
            }
            Some("tools/call") => {
                state.tool_call_count += 1;
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": "called"}],
                        "isError": false
                    }
                }))
                .into_response()
            }
            _ => StatusCode::BAD_REQUEST.into_response(),
        }
    }

    async fn shutdown_handler() -> StatusCode {
        StatusCode::NO_CONTENT
    }

    fn tool(value: Value) -> Tool {
        serde_json::from_value(value).expect("valid rmcp tool fixture")
    }

    #[test]
    fn public_ip_filter_rejects_private_and_special_ranges() {
        assert!(!auth::is_public_network_ip("127.0.0.1".parse().unwrap()));
        assert!(!auth::is_public_network_ip("10.0.0.1".parse().unwrap()));
        assert!(!auth::is_public_network_ip("169.254.1.1".parse().unwrap()));
        assert!(!auth::is_public_network_ip("::1".parse().unwrap()));
        assert!(!auth::is_public_network_ip("fe80::1".parse().unwrap()));
        assert!(!auth::is_public_network_ip(
            "::ffff:127.0.0.1".parse().unwrap()
        ));
        assert!(auth::is_public_network_ip("1.1.1.1".parse().unwrap()));
        assert!(auth::is_public_network_ip(
            "2606:4700:4700::1111".parse().unwrap()
        ));
    }

    #[test]
    fn retired_trusted_header_policy_fails_closed() {
        assert!(
            ConfiguratorTrustedHeaderPolicy::parse(Some("http://127.0.0.1:18081/mcp")).is_err()
        );
        assert!(
            !ConfiguratorTrustedHeaderPolicy
                .permits("lightspeed-configurator", "http://127.0.0.1:18081/mcp")
        );
    }

    #[test]
    fn trusted_header_transport_auth_excludes_bearer() {
        let universe_id = uuid::Uuid::from_u128(7);
        let config = transport_auth(
            StreamableHttpClientTransportConfig::with_uri("http://127.0.0.1:18081/mcp"),
            None,
            Some(universe_id),
        )
        .expect("trusted header config");
        let expected = universe_id.to_string();
        assert_eq!(
            config
                .custom_headers
                .get(&HeaderName::from_static("x-lightspeed-universe"))
                .and_then(|value| value.to_str().ok()),
            Some(expected.as_str())
        );
        assert!(config.auth_header.is_none());
        assert!(
            transport_auth(
                StreamableHttpClientTransportConfig::with_uri("http://127.0.0.1:18081/mcp"),
                Some(&SecretValue::new("not-sent")),
                Some(universe_id),
            )
            .is_err()
        );
    }

    #[test]
    fn discovery_gate_rejects_overlap_and_short_cooldown() {
        let gate = McpDiscoveryGate::new(Duration::from_millis(20));
        let permit = gate.try_start("crm").expect("first discovery");
        assert!(gate.try_start("crm").is_err());
        assert!(gate.try_start("other").is_ok());
        drop(permit);
        assert!(gate.try_start("crm").is_err());
        std::thread::sleep(Duration::from_millis(25));
        assert!(gate.try_start("crm").is_ok());
    }

    #[test]
    fn tool_projection_uses_protocol_title_precedence_and_hints() {
        let tool = project_tool(
            tool(json!({
                "name": "search",
                "title": "Direct title",
                "description": "Search things",
                "inputSchema": {"type": "object"},
                "annotations": {
                    "title": "Annotation title",
                    "readOnlyHint": true,
                    "destructiveHint": false
                }
            })),
            McpToolDiscoveryLimits::default(),
        )
        .expect("valid tool");
        assert_eq!(tool.title.as_deref(), Some("Direct title"));
        let annotations = tool.annotations.expect("annotations");
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.destructive_hint, Some(false));
    }

    #[test]
    fn tool_projection_truncates_verbose_metadata_without_losing_the_tool() {
        let tool = project_tool(
            tool(json!({
                "name": "search",
                "title": "A very long title",
                "description": format!("Notion {}", "雪".repeat(32)),
                "inputSchema": {"type": "object"},
                "annotations": {"title": "An equally long annotation title"}
            })),
            McpToolDiscoveryLimits {
                max_text_bytes: 17,
                ..McpToolDiscoveryLimits::default()
            },
        )
        .expect("verbose hints must not reject an otherwise valid tool");
        assert_eq!(tool.name, "search");
        assert!(tool.title.as_ref().is_some_and(|title| title.len() <= 17));
        assert!(
            tool.description
                .as_ref()
                .is_some_and(|description| description.len() <= 17)
        );
        assert!(tool.description.unwrap().ends_with('…'));
    }

    #[test]
    fn tool_projection_rejects_oversized_schema() {
        let value = tool(json!({
            "name": "search",
            "inputSchema": {"type": "object", "description": "x".repeat(128)}
        }));
        let error = project_tool(
            value,
            McpToolDiscoveryLimits {
                max_schema_bytes: 64,
                ..McpToolDiscoveryLimits::default()
            },
        )
        .expect_err("oversized schema");
        assert_eq!(error.kind, FailureKind::ResponseTooLarge);
    }

    #[test]
    fn tool_call_response_limit_diagnostic_names_tool_call() {
        let client =
            BoundedReqwestClient::new(reqwest::Client::new(), 1, ResponseOperation::ToolCall);
        let _ = client.response_too_large();
        let failure = client.diagnostics.current().expect("diagnostic");
        assert_eq!(failure.kind, FailureKind::ResponseTooLarge);
        assert!(failure.message.contains("tool call"), "{}", failure.message);
        assert!(
            !failure.message.contains("discovery"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn legacy_bootstrap_retry_statuses_exclude_auth_rate_limits_and_server_failures() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::NOT_FOUND,
            StatusCode::METHOD_NOT_ALLOWED,
            StatusCode::NOT_ACCEPTABLE,
            StatusCode::CONFLICT,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            StatusCode::UNPROCESSABLE_ENTITY,
            StatusCode::NOT_IMPLEMENTED,
        ] {
            assert!(is_legacy_bootstrap_rejection(status), "{status}");
        }
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(!is_legacy_bootstrap_rejection(status), "{status}");
        }
    }

    #[test]
    fn duplicate_tools_are_rejected_across_pages() {
        let value = json!({"name": "search", "inputSchema": {"type": "object"}});
        let mut tools = Vec::new();
        let mut seen = BTreeSet::new();
        append_tools(
            vec![tool(value.clone())],
            &mut tools,
            &mut seen,
            McpToolDiscoveryLimits::default(),
        )
        .expect("first occurrence");
        let error = append_tools(
            vec![tool(value)],
            &mut tools,
            &mut seen,
            McpToolDiscoveryLimits::default(),
        )
        .expect_err("duplicate occurrence");
        assert_eq!(error.kind, FailureKind::InvalidResponse);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streamable_discovery_is_live_paginated_and_bearer_authenticated() {
        let state = Arc::new(Mutex::new(TestMcpState {
            tools_list_changed: true,
            ..TestMcpState::default()
        }));
        let app = Router::new()
            .route("/mcp", post(streamable_handler).delete(shutdown_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let discoverer = HttpMcpToolDiscoverer::new();
        let token = SecretValue::new("test-token-never-log");
        let endpoint = format!("http://{address}/mcp");

        for _ in 0..2 {
            let inventory = discoverer
                .discover_tools(
                    &endpoint,
                    Some(&token),
                    None,
                    true,
                    McpToolDiscoveryLimits::default(),
                )
                .await
                .expect("discover fixture tools");
            assert!(inventory.tools_list_changed);
            assert_eq!(inventory.ttl_ms, Some(60_000));
            assert_eq!(
                inventory
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["search", "update"]
            );
            assert_eq!(
                inventory.tools[0]
                    .annotations
                    .as_ref()
                    .unwrap()
                    .read_only_hint,
                Some(true)
            );
        }

        let state = state.lock().await;
        assert_eq!(
            state.initialize_count, 2,
            "each request contacts the server"
        );
        assert_eq!(state.initialized_count, 2);
        assert!(
            state
                .authorizations
                .iter()
                .all(|value| value == "Bearer test-token-never-log")
        );
        server.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streamable_discovery_retries_legacy_initialize_after_probe_rejection() {
        let state = Arc::new(Mutex::new(TestMcpState {
            discover_http_status: Some(StatusCode::BAD_REQUEST),
            ..TestMcpState::default()
        }));
        let app = Router::new()
            .route("/mcp", post(streamable_handler).delete(shutdown_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });

        let inventory = HttpMcpToolDiscoverer::new()
            .discover_tools(
                &format!("http://{address}/mcp"),
                None,
                None,
                true,
                McpToolDiscoveryLimits::default(),
            )
            .await
            .expect("legacy retry should discover tools");
        assert_eq!(inventory.tools.len(), 2);

        let state = state.lock().await;
        assert_eq!(state.discover_count, 1);
        assert_eq!(state.initialize_count, 1);
        assert_eq!(state.initialized_count, 1);
        server.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streamable_tool_call_retries_legacy_initialize_after_probe_rejection() {
        let state = Arc::new(Mutex::new(TestMcpState {
            discover_http_status: Some(StatusCode::BAD_REQUEST),
            ..TestMcpState::default()
        }));
        let app = Router::new()
            .route("/mcp", post(streamable_handler).delete(shutdown_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });

        HttpMcpToolDiscoverer::new()
            .call_tool(
                &format!("http://{address}/mcp"),
                None,
                None,
                true,
                McpToolDiscoveryLimits::default(),
                McpToolCallRequest {
                    tool_name: "search".to_owned(),
                    arguments: serde_json::Map::new(),
                },
            )
            .await
            .expect("legacy retry should call the tool");

        let state = state.lock().await;
        assert_eq!(state.discover_count, 1);
        assert_eq!(state.initialize_count, 1);
        assert_eq!(state.initialized_count, 1);
        assert_eq!(state.tool_call_count, 1);
        server.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streamable_discovery_does_not_retry_legacy_after_server_failure() {
        let state = Arc::new(Mutex::new(TestMcpState {
            discover_http_status: Some(StatusCode::INTERNAL_SERVER_ERROR),
            ..TestMcpState::default()
        }));
        let app = Router::new()
            .route("/mcp", post(streamable_handler).delete(shutdown_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });

        let error = HttpMcpToolDiscoverer::new()
            .discover_tools(
                &format!("http://{address}/mcp"),
                None,
                None,
                true,
                McpToolDiscoveryLimits::default(),
            )
            .await
            .expect_err("server failure must not trigger a legacy retry");
        assert_eq!(error.kind, FailureKind::RemoteFailure);
        assert!(error.message.contains("HTTP 500"));

        let state = state.lock().await;
        assert_eq!(state.discover_count, 1);
        assert_eq!(state.initialize_count, 0);
        server.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streamable_discovery_bounds_raw_response_bytes_before_parsing() {
        let state = Arc::new(Mutex::new(TestMcpState {
            oversized_inventory: true,
            ..TestMcpState::default()
        }));
        let app = Router::new()
            .route("/mcp", post(streamable_handler).delete(shutdown_handler))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });

        let error = HttpMcpToolDiscoverer::new()
            .discover_tools(
                &format!("http://{address}/mcp"),
                None,
                None,
                true,
                McpToolDiscoveryLimits {
                    max_response_bytes: 1024,
                    ..McpToolDiscoveryLimits::default()
                },
            )
            .await
            .expect_err("oversized response must fail");
        assert_eq!(error.kind, FailureKind::ResponseTooLarge);
        server.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streamable_discovery_reports_scope_upgrade_challenges() {
        let state = Arc::new(Mutex::new(TestMcpState {
            oauth_challenge: Some(
                r#"Bearer error="insufficient_scope", scope="tools.read tools.write""#.to_owned(),
            ),
            ..TestMcpState::default()
        }));
        let app = Router::new()
            .route("/mcp", post(streamable_handler).delete(shutdown_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });

        let error = HttpMcpToolDiscoverer::new()
            .discover_tools(
                &format!("http://{address}/mcp"),
                Some(&SecretValue::new("expired-token")),
                None,
                true,
                McpToolDiscoveryLimits::default(),
            )
            .await
            .expect_err("scope challenge must require explicit consent");
        assert_eq!(error.kind, FailureKind::AdditionalConsentRequired);
        assert_eq!(error.required_scopes, ["tools.read", "tools.write"]);
        assert_eq!(state.lock().await.authorizations.len(), 1);
        server.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streamable_discovery_applies_one_end_to_end_deadline() {
        let state = Arc::new(Mutex::new(TestMcpState {
            list_delay: Some(Duration::from_millis(50)),
            ..TestMcpState::default()
        }));
        let app = Router::new()
            .route("/mcp", post(streamable_handler).delete(shutdown_handler))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });

        let error = HttpMcpToolDiscoverer {
            public_http_policy: PinnedHttpPolicy::public_only()
                .with_timeout(Duration::from_millis(10)),
            private_http_policy: PinnedHttpPolicy::allowing_private_networks()
                .with_timeout(Duration::from_millis(10)),
            timeout: Duration::from_millis(10),
        }
        .discover_tools(
            &format!("http://{address}/mcp"),
            None,
            None,
            true,
            McpToolDiscoveryLimits::default(),
        )
        .await
        .expect_err("slow discovery must time out");
        assert_eq!(error.kind, FailureKind::Unreachable);
        server.abort();
    }
}
