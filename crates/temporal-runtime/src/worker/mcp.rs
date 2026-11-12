use std::{
    collections::HashMap,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use auth::{PrivateNetworkAccess, SecretValue};
use base64::Engine;
use harness::RemoteMcpToolSpec;
use ipnet::IpNet;
use llm_runtime::{
    McpInventoryError, McpInventoryResolver, RuntimeMcpTool,
    secrets::{SecretResolveError, SecretResolver, resolve_mcp_bearer},
};
use mcp::search::McpToolSearchQuery;
use tokio::sync::Mutex;
use url::Url;

use crate::gateway::service::mcp_discovery::{
    ConfiguratorTrustedHeaderPolicy, DiscoveredMcpInventory, HttpMcpToolDiscoverer, McpRequestAuth,
    McpToolCallRequest, McpToolDiscoverer, resolve_mcp_request_headers,
};

pub enum RuntimeMcpExecutionOutcome {
    Completed {
        output: serde_json::Value,
        visible: String,
        is_error: bool,
        assets: Vec<RuntimeMcpAsset>,
        /// Admitted `content` assets, in content order, that become media
        /// entries after the tool result. Indexes point into `assets`.
        media: Vec<RuntimeMcpMedia>,
    },
    NeedsApproval {
        subject: harness::ApprovalSubject,
    },
}

/// Binary content stripped out of a tool result: `image` and `audio` blocks
/// and embedded `resource` blobs, from `content` first and then from
/// `structuredContent`, in document order.
pub struct RuntimeMcpAsset {
    pub bytes: Vec<u8>,
    /// Content identity of `bytes`; the store returns the same reference.
    pub blob_ref: harness::BlobRef,
    pub media_type: Option<String>,
    pub kind: String,
    /// Resource `uri`, when the asset came from an embedded resource.
    pub uri: Option<String>,
}

/// One admitted asset the model gets to see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeMcpMedia {
    pub asset_index: usize,
    pub kind: harness::media::MediaKind,
    pub media_type: String,
    pub name: Option<String>,
}

#[derive(Clone)]
pub struct RuntimeMcpExecutor {
    servers: Arc<dyn mcp::McpRegistryStore>,
    secrets: Arc<dyn SecretResolver>,
    inventory: Arc<RuntimeMcpInventoryResolver>,
    transport: HttpMcpToolDiscoverer,
    private_networks: McpPrivateNetworkPolicy,
    trusted_header: ConfiguratorTrustedHeaderPolicy,
    universe_id: uuid::Uuid,
}

/// Demand-driven inventory TTL for servers without a tool-list change signal.
pub const MCP_INVENTORY_CACHE_TTL: Duration = Duration::from_secs(300);
/// Short fallback while Lightspeed does not maintain notification subscriptions.
pub const MCP_LIST_CHANGED_CACHE_TTL: Duration = Duration::from_secs(60);
/// How long a failed fetch answers for its server. Without it every turn would
/// fetch again, and concurrent turns would queue behind the per-server lock.
pub const MCP_INVENTORY_FAILURE_TTL: Duration = Duration::from_secs(60);
const MCP_FIND_HIT_MAX_BYTES: usize = 8 * 1024;
const MCP_FIND_PAGE_MAX_BYTES: usize = 64 * 1024;
const MCP_FIND_DETAIL_MAX_NAMES: usize = 5;
const MCP_FIND_TRUNCATED_NOTE: &str =
    "Call mcp_find_tools with server and names for the full definition.";

/// Server, record revision and the attachment's tool subset: two sessions on one
/// server can allow different tools, and each must get its own list.
type InventoryCacheKey = (String, u64, Option<Vec<String>>);
type InventoryLocks = HashMap<InventoryCacheKey, Arc<Mutex<()>>>;

#[derive(Clone, Debug, Default)]
pub struct McpPrivateNetworkPolicy {
    hosts: Arc<Vec<String>>,
    networks: Arc<[IpNet]>,
}

impl McpPrivateNetworkPolicy {
    pub fn from_env() -> Result<Self, String> {
        Self::parse(
            std::env::var("LIGHTSPEED_MCP_PRIVATE_NETWORKS")
                .ok()
                .as_deref(),
        )
    }

    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        let mut hosts = Vec::new();
        let mut networks = Vec::new();
        for item in value
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            if let Ok(network) = IpNet::from_str(item) {
                networks.push(network);
            } else if item.contains('/') {
                return Err(format!(
                    "invalid CIDR in LIGHTSPEED_MCP_PRIVATE_NETWORKS: {item}"
                ));
            } else {
                hosts.push(item.to_ascii_lowercase());
            }
        }
        Ok(Self {
            hosts: Arc::new(hosts),
            networks: networks.into(),
        })
    }

    /// What a request to `server_url` may reach beyond public addresses.
    ///
    /// A listed host is trusted wherever it resolves. Any other host is held
    /// to the listed networks, and that is decided on its resolved addresses
    /// by the client that pins them: only there is a hostname's destination
    /// known, and known to be the one the connection uses.
    pub fn access(&self, server_url: &str, record_opt_in: bool) -> PrivateNetworkAccess {
        if !record_opt_in {
            return PrivateNetworkAccess::None;
        }
        let Ok(url) = Url::parse(server_url) else {
            return PrivateNetworkAccess::None;
        };
        let Some(host) = url.host_str() else {
            return PrivateNetworkAccess::None;
        };
        // A URL writes an IPv6 literal in brackets; the list does not.
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        if self.hosts.contains(&host) {
            return PrivateNetworkAccess::Any;
        }
        if self.networks.is_empty() {
            PrivateNetworkAccess::None
        } else {
            PrivateNetworkAccess::Networks(self.networks.clone())
        }
    }
}

#[derive(Clone)]
pub struct RuntimeMcpInventoryResolver {
    servers: Arc<dyn mcp::McpRegistryStore>,
    discoverer: Arc<dyn McpToolDiscoverer>,
    secrets: Arc<dyn SecretResolver>,
    private_networks: McpPrivateNetworkPolicy,
    trusted_header: ConfiguratorTrustedHeaderPolicy,
    universe_id: uuid::Uuid,
    cache: Arc<Mutex<HashMap<InventoryCacheKey, CachedInventory>>>,
    locks: Arc<Mutex<InventoryLocks>>,
    ttl: Duration,
    list_changed_ttl: Duration,
    failure_ttl: Duration,
}

#[derive(Clone)]
struct CachedInventory {
    fetched_at: Instant,
    ttl: Duration,
    tools: Vec<RuntimeMcpTool>,
}

impl RuntimeMcpInventoryResolver {
    pub(crate) fn new(
        servers: Arc<dyn mcp::McpRegistryStore>,
        secrets: Arc<dyn SecretResolver>,
        private_networks: McpPrivateNetworkPolicy,
        trusted_header: ConfiguratorTrustedHeaderPolicy,
        universe_id: uuid::Uuid,
    ) -> Self {
        Self {
            servers,
            discoverer: Arc::new(HttpMcpToolDiscoverer::new()),
            secrets,
            private_networks,
            trusted_header,
            universe_id,
            cache: Arc::new(Mutex::new(HashMap::new())),
            locks: Arc::new(Mutex::new(HashMap::new())),
            ttl: MCP_INVENTORY_CACHE_TTL,
            list_changed_ttl: MCP_LIST_CHANGED_CACHE_TTL,
            failure_ttl: MCP_INVENTORY_FAILURE_TTL,
        }
    }

    #[cfg(test)]
    fn for_test(
        discoverer: Arc<dyn McpToolDiscoverer>,
        ttl: Duration,
        list_changed_ttl: Duration,
    ) -> Self {
        Self {
            servers: Arc::new(tests::OneServer),
            discoverer,
            secrets: Arc::new(llm_runtime::secrets::AbsentSecretResolver),
            private_networks: McpPrivateNetworkPolicy::parse(Some("127.0.0.1"))
                .expect("test private policy"),
            trusted_header: ConfiguratorTrustedHeaderPolicy,
            universe_id: uuid::Uuid::nil(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            locks: Arc::new(Mutex::new(HashMap::new())),
            ttl,
            list_changed_ttl,
            failure_ttl: list_changed_ttl,
        }
    }

    async fn uncached(
        &self,
        spec: &RemoteMcpToolSpec,
    ) -> Result<(Vec<RuntimeMcpTool>, bool, Option<Duration>), McpInventoryError> {
        let record = usable_record(self.servers.as_ref(), &spec.server_id)
            .await
            .map_err(McpInventoryError::new)?
            .ok_or_else(|| {
                McpInventoryError::new(format!("MCP server {} is not available", spec.server_id))
            })?;
        let trusted_universe = self
            .trusted_header
            .permits(&spec.server_id, &record.server_url)
            .then_some(self.universe_id);
        let auth = record_auth(self.secrets.as_ref(), &record, trusted_universe.is_some())
            .await
            .map_err(|error| McpInventoryError::new(format!("resolve MCP credential: {error}")))?;
        let private_access = self
            .private_networks
            .access(&record.server_url, record.allow_private_network);
        let discovered = self
            .discoverer
            .discover_tools(
                &record.server_url,
                &auth,
                trusted_universe,
                private_access,
                mcp::McpToolDiscoveryLimits::default(),
            )
            .await
            .map_err(|error| McpInventoryError::new(error.to_string()))?;
        let DiscoveredMcpInventory {
            tools: discovered,
            tools_list_changed,
            ttl_ms,
        } = discovered;
        let mut tools = discovered
            .into_iter()
            .filter(|tool| {
                spec.allowed_tools
                    .as_ref()
                    .is_none_or(|allowed| allowed.iter().any(|candidate| candidate == &tool.name))
            })
            .map(|tool| RuntimeMcpTool {
                remote_name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
                output_schema: tool.output_schema,
                annotations: tool.annotations.map(mcp_tool_annotations),
            })
            .collect::<Vec<_>>();
        tools.sort_by(|left, right| left.remote_name.cmp(&right.remote_name));
        Ok((tools, tools_list_changed, ttl_ms.map(Duration::from_millis)))
    }
}

/// A server's live record, which decides its URL, network opt-in and credential at
/// each use, so an admin's change applies at once. `None` when it is gone or
/// switched off.
async fn usable_record(
    servers: &dyn mcp::McpRegistryStore,
    server_id: &str,
) -> Result<Option<mcp::McpServerRecord>, String> {
    let id = mcp::McpServerId::try_new(server_id.to_owned())
        .map_err(|error| format!("invalid MCP server id: {error}"))?;
    match servers.read_server(&id).await {
        Ok(record)
            if matches!(
                record.status,
                mcp::McpServerStatus::Active | mcp::McpServerStatus::Unverified
            ) =>
        {
            Ok(Some(record))
        }
        Ok(_) | Err(mcp::McpRegistryError::NotFound { .. }) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// The live record's bearer and extra headers. A trusted-header server takes
/// no bearer but still receives its configured headers.
async fn record_auth(
    secrets: &dyn SecretResolver,
    record: &mcp::McpServerRecord,
    trusted: bool,
) -> Result<McpRequestAuth, SecretResolveError> {
    let bearer = if trusted {
        None
    } else {
        resolve_mcp_bearer(secrets, record.server_id.as_str(), &record.server_url)
            .await?
            .map(|value| SecretValue::new(value.expose()))
    };
    let headers = resolve_mcp_request_headers(secrets, record).await?;
    Ok(McpRequestAuth { bearer, headers })
}

impl RuntimeMcpExecutor {
    /// Use the same injected inventory and naming rules as the model adapters
    /// when preparing an execution-scoped callable namespace.
    pub(crate) async fn injected_tools(
        &self,
        spec: &RemoteMcpToolSpec,
        server_name: &harness::ToolName,
        request_tool_count: &mut usize,
    ) -> Result<Vec<(String, RuntimeMcpTool)>, llm_runtime::LlmAdapterError> {
        llm_runtime::injected_runtime_tools(
            self.inventory.as_ref(),
            spec,
            server_name,
            request_tool_count,
        )
        .await
    }

    pub(crate) fn new(
        servers: Arc<dyn mcp::McpRegistryStore>,
        secrets: Arc<dyn SecretResolver>,
        inventory: Arc<RuntimeMcpInventoryResolver>,
        private_networks: McpPrivateNetworkPolicy,
        trusted_header: ConfiguratorTrustedHeaderPolicy,
        universe_id: uuid::Uuid,
    ) -> Self {
        Self {
            servers,
            secrets,
            inventory,
            transport: HttpMcpToolDiscoverer::new(),
            private_networks,
            trusted_header,
            universe_id,
        }
    }

    pub async fn execute(
        &self,
        request: &harness::ToolInvocationCallRequest,
        arguments: serde_json::Value,
    ) -> Result<RuntimeMcpExecutionOutcome, String> {
        let runtime = request
            .call
            .remote_mcp
            .as_ref()
            .ok_or_else(|| "runtime MCP routing facts are missing".to_owned())?;
        match runtime {
            harness::RemoteMcpCallRuntime::Injected {
                target,
                remote_tool_name,
                approval_decision,
            } => {
                let arguments = arguments
                    .as_object()
                    .cloned()
                    .ok_or_else(|| "runtime MCP tool arguments must be a JSON object".to_owned())?;
                self.call(
                    request,
                    target,
                    remote_tool_name,
                    arguments,
                    *approval_decision,
                )
                .await
            }
            harness::RemoteMcpCallRuntime::Search {
                targets,
                approval_decision,
            } if request
                .call
                .tool_id
                .as_ref()
                .is_some_and(|id| id.as_str() == harness::MCP_FIND_TOOLS_TOOL_ID) =>
            {
                self.find_tools(targets, arguments).await
            }
            harness::RemoteMcpCallRuntime::Search {
                targets,
                approval_decision,
            } if request
                .call
                .tool_id
                .as_ref()
                .is_some_and(|id| id.as_str() == harness::MCP_CALL_TOOL_ID) =>
            {
                let object = arguments
                    .as_object()
                    .ok_or_else(|| "mcp_call arguments must be a JSON object".to_owned())?;
                let server = object
                    .get("server")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "mcp_call requires string server".to_owned())?;
                let tool = object
                    .get("tool")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "mcp_call requires string tool".to_owned())?;
                let inner = object
                    .get("arguments")
                    .and_then(serde_json::Value::as_object)
                    .cloned()
                    .ok_or_else(|| "mcp_call requires object arguments".to_owned())?;
                let target = targets
                    .iter()
                    .find(|target| target.server_id == server)
                    .ok_or_else(|| format!("unknown active search-exposure MCP server {server}"))?;
                self.validate_search_call(target, tool, &inner).await?;
                self.call(request, target, tool, inner, *approval_decision)
                    .await
            }
            harness::RemoteMcpCallRuntime::Search { .. } => {
                Err("unknown runtime MCP search meta-tool".to_owned())
            }
        }
    }

    async fn find_tools(
        &self,
        targets: &[harness::RemoteMcpCallTarget],
        arguments: serde_json::Value,
    ) -> Result<RuntimeMcpExecutionOutcome, String> {
        let object = arguments
            .as_object()
            .ok_or_else(|| "mcp_find_tools arguments must be a JSON object".to_owned())?;
        let selected_server = object.get("server").and_then(serde_json::Value::as_str);
        if let Some(server) = selected_server
            && !targets.iter().any(|target| target.server_id == server)
        {
            return Err(format!(
                "unknown active search-exposure MCP server {server}"
            ));
        }
        let names = match object.get("names") {
            None => None,
            Some(serde_json::Value::Array(names)) => {
                if names.is_empty() {
                    return Err("mcp_find_tools names must not be empty".to_owned());
                }
                if names.len() > MCP_FIND_DETAIL_MAX_NAMES {
                    return Err(format!(
                        "mcp_find_tools accepts at most {MCP_FIND_DETAIL_MAX_NAMES} names"
                    ));
                }
                Some(
                    names
                        .iter()
                        .map(|name| {
                            name.as_str().ok_or_else(|| {
                                "mcp_find_tools names must contain only strings".to_owned()
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            Some(_) => return Err("mcp_find_tools names must be an array of strings".to_owned()),
        };
        let query = object
            .get("query")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(McpToolSearchQuery::new);
        if let Some(names) = names {
            let server =
                selected_server.ok_or_else(|| "mcp_find_tools names requires server".to_owned())?;
            if object.contains_key("query") || object.contains_key("cursor") {
                return Err("mcp_find_tools detail mode does not accept query or cursor".to_owned());
            }
            let target = targets
                .iter()
                .find(|target| target.server_id == server)
                .expect("selected server was validated");
            let spec = spec_from_target(target, harness::RemoteMcpExposure::Search);
            let inventory = self
                .inventory
                .list_tools(&spec)
                .await
                .map_err(|error| error.to_string())?;
            let mut tools = Vec::with_capacity(names.len());
            for name in names {
                let tool = inventory
                    .iter()
                    .find(|tool| tool.remote_name == name)
                    .ok_or_else(|| format!("MCP server {server} does not advertise tool {name}"))?;
                tools.push(full_tool_definition(server, tool));
            }
            let output = serde_json::json!({"tools": tools, "nextCursor": null});
            return Ok(RuntimeMcpExecutionOutcome::Completed {
                visible: serde_json::to_string(&output).map_err(|error| error.to_string())?,
                output,
                is_error: false,
                assets: Vec::new(),
                media: Vec::new(),
            });
        }
        let cursor = object
            .get("cursor")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize;
        let mut matches = Vec::new();
        for target in targets
            .iter()
            .filter(|target| selected_server.is_none_or(|server| server == target.server_id))
        {
            let spec = spec_from_target(target, harness::RemoteMcpExposure::Search);
            for tool in self
                .inventory
                .list_tools(&spec)
                .await
                .map_err(|error| error.to_string())?
            {
                let rank = match query.as_ref() {
                    Some(query) => {
                        let Some(rank) = query.score(
                            &tool.remote_name,
                            tool.description.as_deref(),
                            &tool.input_schema,
                        ) else {
                            continue;
                        };
                        Some(rank)
                    }
                    None => None,
                };
                matches.push((rank, target.server_id.clone(), tool));
            }
        }
        matches.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.2.remote_name.cmp(&right.2.remote_name))
        });
        let output = paged_tool_output(&matches, cursor)?;
        Ok(RuntimeMcpExecutionOutcome::Completed {
            visible: serde_json::to_string(&output).map_err(|error| error.to_string())?,
            output,
            is_error: false,
            assets: Vec::new(),
            media: Vec::new(),
        })
    }

    async fn validate_search_call(
        &self,
        target: &harness::RemoteMcpCallTarget,
        tool_name: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        if target
            .allowed_tools
            .as_ref()
            .is_some_and(|allowed| !allowed.iter().any(|candidate| candidate == tool_name))
        {
            return Err(format!(
                "MCP tool {tool_name} is not allowed on {}",
                target.server_id
            ));
        }
        let spec = spec_from_target(target, harness::RemoteMcpExposure::Search);
        let tools = self
            .inventory
            .list_tools(&spec)
            .await
            .map_err(|error| error.to_string())?;
        let tool = tools
            .iter()
            .find(|tool| tool.remote_name == tool_name)
            .ok_or_else(|| {
                format!(
                    "MCP server {} does not advertise tool {tool_name}",
                    target.server_id
                )
            })?;
        validate_mcp_arguments(tool_name, tool, arguments)
    }

    async fn call(
        &self,
        request: &harness::ToolInvocationCallRequest,
        target: &harness::RemoteMcpCallTarget,
        tool_name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
        approval_decision: Option<bool>,
    ) -> Result<RuntimeMcpExecutionOutcome, String> {
        if target.approval == harness::RemoteMcpApprovalPolicy::Always {
            match approval_decision {
                None => {
                    return Ok(RuntimeMcpExecutionOutcome::NeedsApproval {
                        subject: harness::ApprovalSubject::McpToolCall {
                            server_id: target.server_id.clone(),
                            server_label: target.server_label.clone(),
                            tool_name: tool_name.to_owned(),
                            arguments_ref: request.call.arguments_ref.clone(),
                        },
                    });
                }
                Some(false) => {
                    let output = serde_json::json!({
                        "error": "MCP tool call was rejected",
                        "server": target.server_id,
                        "tool": tool_name,
                    });
                    return Ok(RuntimeMcpExecutionOutcome::Completed {
                        visible: serde_json::to_string(&output)
                            .map_err(|error| error.to_string())?,
                        output,
                        is_error: true,
                        assets: Vec::new(),
                        media: Vec::new(),
                    });
                }
                Some(true) => {}
            }
        }
        let record = self.runtime_record(target).await?;
        let trusted_universe = self
            .trusted_header
            .permits(&target.server_id, &record.server_url)
            .then_some(self.universe_id);
        let auth = record_auth(self.secrets.as_ref(), &record, trusted_universe.is_some())
            .await
            .map_err(|error| format!("resolve MCP credential: {error}"))?;
        let private_access = self
            .private_networks
            .access(&record.server_url, record.allow_private_network);
        let result = self
            .transport
            .call_tool(
                &record.server_url,
                &auth,
                trusted_universe,
                private_access,
                mcp::McpToolDiscoveryLimits::default(),
                McpToolCallRequest {
                    tool_name: tool_name.to_owned(),
                    arguments,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        let raw = serde_json::to_value(&result).map_err(|error| error.to_string())?;
        let mut output = serde_json::json!({
            "content": raw.get("content").cloned().unwrap_or_default(),
        });
        if let Some(structured) = result.structured_content {
            output
                .as_object_mut()
                .expect("runtime MCP output object")
                .insert("structuredContent".to_owned(), structured);
        }
        let mut assets = Vec::new();
        // `content` assets come first so their indexes line up with the
        // content blocks the visible text labels.
        if let Some(content) = output.get_mut("content") {
            extract_mcp_assets(content, &mut assets)?;
        }
        if let Some(structured) = output.get_mut("structuredContent") {
            extract_mcp_assets(structured, &mut assets)?;
        }
        let (visible, media) = visible_mcp_result(&raw, &output, &assets);
        Ok(RuntimeMcpExecutionOutcome::Completed {
            output,
            visible,
            is_error: result.is_error.unwrap_or(false),
            assets,
            media,
        })
    }

    /// The record a runtime call runs against. Its errors become model-visible
    /// failed results, so they never carry the server's URL, which can hold a token.
    async fn runtime_record(
        &self,
        target: &harness::RemoteMcpCallTarget,
    ) -> Result<mcp::McpServerRecord, String> {
        let record = usable_record(self.servers.as_ref(), &target.server_id)
            .await?
            .ok_or_else(|| format!("MCP server {} is not available", target.server_id))?;
        if record.execution != mcp::McpExecution::Runtime {
            return Err(format!(
                "MCP server {} is no longer configured for runtime execution",
                target.server_id
            ));
        }
        Ok(record)
    }
}

fn mcp_tool_annotations(annotations: mcp::McpToolAnnotations) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    for (name, hint) in [
        ("readOnlyHint", annotations.read_only_hint),
        ("destructiveHint", annotations.destructive_hint),
        ("idempotentHint", annotations.idempotent_hint),
        ("openWorldHint", annotations.open_world_hint),
    ] {
        if let Some(hint) = hint {
            value.insert(name.to_owned(), serde_json::Value::Bool(hint));
        }
    }
    serde_json::Value::Object(value)
}

fn validate_mcp_arguments(
    tool_name: &str,
    tool: &RuntimeMcpTool,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let schema = serde_json::to_string(&tool.input_schema).map_err(|error| error.to_string())?;
    let validator = jsonschema::validator_for(&tool.input_schema).map_err(|error| {
        format!("MCP tool {tool_name} has an invalid input schema: {error}; input schema: {schema}")
    })?;
    validator
        .validate(&serde_json::Value::Object(arguments.clone()))
        .map_err(|error| {
            format!(
                "arguments for MCP tool {tool_name} do not match its schema: {error}; input schema: {schema}"
            )
        })
}

fn full_tool_definition(server: &str, tool: &RuntimeMcpTool) -> serde_json::Value {
    let mut definition = serde_json::json!({
        "server": server,
        "name": tool.remote_name,
        "description": tool.description,
        "inputSchema": tool.input_schema,
        "annotations": tool.annotations,
    });
    if let Some(schema) = &tool.output_schema {
        definition["outputSchema"] = schema.clone();
    }
    definition
}

fn compact_search_hit(server: &str, tool: &RuntimeMcpTool) -> Result<serde_json::Value, String> {
    let full = full_tool_definition(server, tool);
    if serde_json::to_vec(&full)
        .map_err(|error| error.to_string())?
        .len()
        <= MCP_FIND_HIT_MAX_BYTES
    {
        return Ok(full);
    }

    let mut hit = serde_json::json!({
        "server": server,
        "name": tool.remote_name,
        "description": tool.description.as_ref().map(|_| ""),
        "inputSchema": tool.input_schema,
        "annotations": tool.annotations,
        "truncated": MCP_FIND_TRUNCATED_NOTE,
    });

    if let Some(schema) = &tool.output_schema {
        hit["outputSchema"] = schema.clone();
        // Search hits may omit whole schemas to stay bounded. Never truncate
        // a schema into a different contract; the full definition keeps it.
        if serialized_len(&hit)? > MCP_FIND_HIT_MAX_BYTES {
            hit.as_object_mut()
                .expect("MCP search hit object")
                .remove("outputSchema");
        }
    }

    if serialized_len(&hit)? > MCP_FIND_HIT_MAX_BYTES {
        let mut argument_names = tool
            .input_schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flat_map(|properties| properties.keys().cloned())
            .collect::<Vec<_>>();
        argument_names.sort();
        hit["inputSchema"] = serde_json::json!(argument_names);

        // The schema bounds normally make the complete argument-name list fit.
        // Keep the hit invariant defensive even for an adversarial schema with
        // thousands of long top-level properties.
        while serialized_len(&hit)? > MCP_FIND_HIT_MAX_BYTES {
            let Some(names) = hit["inputSchema"].as_array_mut() else {
                break;
            };
            if names.pop().is_none() {
                break;
            }
        }
    }

    if let Some(description) = tool.description.as_deref() {
        let mut boundaries = description
            .char_indices()
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        boundaries.push(description.len());
        boundaries.sort_unstable();
        boundaries.dedup();

        let mut low = 0;
        let mut high = boundaries.len();
        while low < high {
            let middle = (low + high) / 2;
            hit["description"] =
                serde_json::Value::String(description[..boundaries[middle]].to_owned());
            if serialized_len(&hit)? <= MCP_FIND_HIT_MAX_BYTES {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let selected = low.saturating_sub(1);
        hit["description"] =
            serde_json::Value::String(description[..boundaries[selected]].to_owned());
    }

    if serialized_len(&hit)? > MCP_FIND_HIT_MAX_BYTES {
        return Err("MCP tool identity exceeds the search hit byte limit".to_owned());
    }
    Ok(hit)
}

fn paged_tool_output(
    matches: &[(
        Option<mcp::search::McpToolSearchScore>,
        String,
        RuntimeMcpTool,
    )],
    cursor: usize,
) -> Result<serde_json::Value, String> {
    let mut page = Vec::new();
    for (_, server, tool) in matches.iter().skip(cursor) {
        page.push(compact_search_hit(server, tool)?);
        let next_cursor = (cursor + page.len() < matches.len()).then_some(cursor + page.len());
        let candidate = serde_json::json!({"tools": page, "nextCursor": next_cursor});
        if serialized_len(&candidate)? > MCP_FIND_PAGE_MAX_BYTES && page.len() > 1 {
            page.pop();
            break;
        }
    }
    let next_cursor = (cursor + page.len() < matches.len()).then_some(cursor + page.len());
    let output = serde_json::json!({"tools": page, "nextCursor": next_cursor});
    debug_assert!(serialized_len(&output).is_ok_and(|length| length <= MCP_FIND_PAGE_MAX_BYTES));
    Ok(output)
}

fn serialized_len(value: &serde_json::Value) -> Result<usize, String> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|error| error.to_string())
}

fn extract_mcp_assets(
    value: &mut serde_json::Value,
    assets: &mut Vec<RuntimeMcpAsset>,
) -> Result<(), String> {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                extract_mcp_assets(value, assets)?;
            }
        }
        serde_json::Value::Object(object) => {
            let kind = object
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            if matches!(kind.as_deref(), Some("image" | "audio"))
                && let Some(encoded) = object
                    .remove("data")
                    .and_then(|value| value.as_str().map(str::to_owned))
            {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|error| format!("MCP {kind:?} content is invalid base64: {error}"))?;
                let index = assets.len();
                assets.push(RuntimeMcpAsset {
                    blob_ref: harness::BlobRef::from_bytes(&bytes),
                    bytes,
                    media_type: object
                        .get("mimeType")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    kind: kind.clone().expect("matched content kind"),
                    uri: None,
                });
                object.insert("blobIndex".to_owned(), serde_json::json!(index));
            }
            if kind.as_deref() == Some("resource")
                && let Some(resource) = object
                    .get_mut("resource")
                    .and_then(serde_json::Value::as_object_mut)
                && let Some(encoded) = resource
                    .remove("blob")
                    .and_then(|value| value.as_str().map(str::to_owned))
            {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|error| format!("MCP resource content is invalid base64: {error}"))?;
                let index = assets.len();
                assets.push(RuntimeMcpAsset {
                    blob_ref: harness::BlobRef::from_bytes(&bytes),
                    bytes,
                    media_type: resource
                        .get("mimeType")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    kind: "resource".to_owned(),
                    uri: resource
                        .get("uri")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                });
                resource.insert("blobIndex".to_owned(), serde_json::json!(index));
            }
            for value in object.values_mut() {
                extract_mcp_assets(value, assets)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn attach_mcp_asset_refs(value: &mut serde_json::Value, refs: &[harness::BlobRef]) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                attach_mcp_asset_refs(value, refs);
            }
        }
        serde_json::Value::Object(object) => {
            if let Some(index) = object.remove("blobIndex").and_then(|value| value.as_u64())
                && let Some(blob_ref) = refs.get(index as usize)
            {
                object.insert(
                    "blobRef".to_owned(),
                    serde_json::Value::String(blob_ref.as_str().to_owned()),
                );
            }
            for value in object.values_mut() {
                attach_mcp_asset_refs(value, refs);
            }
        }
        _ => {}
    }
}

fn spec_from_target(
    target: &harness::RemoteMcpCallTarget,
    exposure: harness::RemoteMcpExposure,
) -> RemoteMcpToolSpec {
    RemoteMcpToolSpec {
        server_id: target.server_id.clone(),
        record_revision: target.record_revision,
        server_label: target.server_label.clone(),
        description_ref: None,
        allowed_tools: target.allowed_tools.clone(),
        execution: harness::RemoteMcpExecution::Runtime,
        exposure,
        approval: target.approval.clone(),
        defer_loading: None,
    }
}

/// The model-visible text of a tool result plus the assets the model gets to
/// see. Text blocks pass through. Every binary block is announced by position
/// and handle (`[image 1 · media:3f9a2c1d4e7b · image/png · 38 KiB]`) when
/// admitted, or dropped with its reason (`[image 2 omitted: image/svg+xml is
/// not supported]`); tool-produced media never fails the run. Embedded text
/// resources and resource links are rendered inline since the structured
/// output is not model-visible.
fn visible_mcp_result(
    raw: &serde_json::Value,
    fallback: &serde_json::Value,
    assets: &[RuntimeMcpAsset],
) -> (String, Vec<RuntimeMcpMedia>) {
    let mut lines = Vec::new();
    let mut media = Vec::new();
    let mut next_asset = 0usize;
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut next_index = |label: &'static str| -> usize {
        let count = counts.entry(label).or_insert(0);
        *count += 1;
        *count
    };
    let blocks = raw
        .get("content")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten();
    for block in blocks {
        let kind = block.get("type").and_then(serde_json::Value::as_str);
        let mime = |value: &serde_json::Value| {
            value
                .get("mimeType")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        match kind {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(serde_json::Value::as_str) {
                    lines.push(text.to_owned());
                }
            }
            Some(label @ ("image" | "audio")) if block.get("data").is_some() => {
                let asset_index = next_asset;
                next_asset += 1;
                let Some(asset) = assets.get(asset_index) else {
                    continue;
                };
                let index = next_index(if label == "image" { "image" } else { "audio" });
                lines.push(render_mcp_asset(
                    asset,
                    asset_index,
                    label,
                    index,
                    None,
                    &mut media,
                ));
            }
            Some("resource") => {
                let Some(resource) = block.get("resource") else {
                    lines.push("[MCP resource content stored as structured output]".to_owned());
                    continue;
                };
                let uri = resource.get("uri").and_then(serde_json::Value::as_str);
                if let Some(text) = resource.get("text").and_then(serde_json::Value::as_str) {
                    match uri {
                        Some(uri) => lines.push(format!("[resource: {uri}]\n{text}")),
                        None => lines.push(text.to_owned()),
                    }
                    continue;
                }
                if resource.get("blob").is_none() {
                    lines.push("[MCP resource content stored as structured output]".to_owned());
                    continue;
                }
                let asset_index = next_asset;
                next_asset += 1;
                let Some(asset) = assets.get(asset_index) else {
                    continue;
                };
                let name = uri.map(resource_name);
                let index = next_index("document");
                lines.push(render_mcp_asset(
                    asset,
                    asset_index,
                    "document",
                    index,
                    name,
                    &mut media,
                ));
            }
            Some("resource_link") => {
                let uri = block
                    .get("uri")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                match mime(block) {
                    Some(mime) => lines.push(format!("[resource_link: {uri} · {mime}]")),
                    None => lines.push(format!("[resource_link: {uri}]")),
                }
            }
            Some(other) => {
                lines.push(format!("[MCP {other} content stored as structured output]"));
            }
            None => {}
        }
    }
    let visible = if lines.is_empty() {
        serde_json::to_string(fallback).unwrap_or_else(|_| "MCP tool returned a result".to_owned())
    } else {
        lines.join("\n")
    };
    (visible, media)
}

/// Admit and render one binary block against the shared result-wide media cap.
fn render_mcp_asset(
    asset: &RuntimeMcpAsset,
    asset_index: usize,
    label: &str,
    index: usize,
    name: Option<String>,
    media: &mut Vec<RuntimeMcpMedia>,
) -> String {
    use harness::media::{
        MediaRejection, admit_tool_media, tool_media_line, tool_media_omitted_line,
    };

    let admission = if label == "audio" {
        Err(MediaRejection::UnsupportedMediaType {
            media_type: asset
                .media_type
                .clone()
                .unwrap_or_else(|| "audio".to_owned()),
        })
    } else {
        admit_tool_media(asset.media_type.as_deref(), asset.bytes.len() as u64)
    };
    let admission = admission.and_then(|kind| {
        if media.len() >= harness::media::MAX_TOOL_MEDIA_ITEMS {
            Err(MediaRejection::TooMany {
                limit: harness::media::MAX_TOOL_MEDIA_ITEMS,
            })
        } else {
            Ok(kind)
        }
    });
    match admission {
        Ok(kind) => {
            let media_type = harness::media::normalized_media_type(
                asset.media_type.as_deref().unwrap_or_default(),
            );
            let line = tool_media_line(
                kind,
                index,
                &asset.blob_ref,
                &media_type,
                name.as_deref(),
                asset.bytes.len() as u64,
            );
            media.push(RuntimeMcpMedia {
                asset_index,
                kind,
                media_type,
                name,
            });
            line
        }
        Err(rejection) => tool_media_omitted_line(label, index, &rejection),
    }
}

/// The last path segment of a resource URI, used as a document name.
fn resource_name(uri: &str) -> String {
    let trimmed = uri.trim_end_matches('/');
    let name = trimmed.rsplit(['/', ':']).next().unwrap_or(trimmed);
    if name.is_empty() {
        uri.to_owned()
    } else {
        name.to_owned()
    }
}

#[async_trait]
impl McpInventoryResolver for RuntimeMcpInventoryResolver {
    async fn list_tools(
        &self,
        spec: &RemoteMcpToolSpec,
    ) -> Result<Vec<RuntimeMcpTool>, McpInventoryError> {
        let key = (
            spec.server_id.clone(),
            spec.record_revision,
            spec.allowed_tools.clone(),
        );
        if let Some(cached) = self.cache.lock().await.get(&key)
            && cached.fetched_at.elapsed() < cached.ttl
        {
            return Ok(cached.tools.clone());
        }
        let lock = {
            let mut locks = self.locks.lock().await;
            locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _permit = lock.lock().await;
        if let Some(cached) = self.cache.lock().await.get(&key)
            && cached.fetched_at.elapsed() < cached.ttl
        {
            return Ok(cached.tools.clone());
        }
        let (tools, tools_list_changed, advertised_ttl) = match self.uncached(spec).await {
            Ok(fetched) => fetched,
            // A server that cannot list its tools never fails the turn. Keep the
            // last list, or none, and ask again after the failure TTL. An old list
            // grants nothing, since each call re-reads the record and credential.
            Err(error) => {
                tracing::warn!(
                    server_id = %spec.server_id,
                    %error,
                    "MCP tools/list failed; keeping the last inventory"
                );
                let mut cache = self.cache.lock().await;
                let entry = cache.entry(key).or_insert_with(|| CachedInventory {
                    fetched_at: Instant::now(),
                    ttl: self.failure_ttl,
                    tools: Vec::new(),
                });
                entry.fetched_at = Instant::now();
                entry.ttl = self.failure_ttl;
                return Ok(entry.tools.clone());
            }
        };
        let ttl = advertised_ttl.unwrap_or(if tools_list_changed {
            self.list_changed_ttl
        } else {
            self.ttl
        });
        let mut cache = self.cache.lock().await;
        cache.retain(|(server_id, revision, _), _| {
            server_id != &spec.server_id || *revision == spec.record_revision
        });
        cache.insert(
            key,
            CachedInventory {
                fetched_at: Instant::now(),
                ttl,
                tools: tools.clone(),
            },
        );
        Ok(tools)
    }

    async fn server_url(&self, server_id: &str) -> Result<Option<String>, McpInventoryError> {
        // A record moved to the runtime can name a private server and hold a credential
        // meant for Lightspeed, neither of which should reach a model provider.
        Ok(usable_record(self.servers.as_ref(), server_id)
            .await
            .map_err(McpInventoryError::transient)?
            .filter(|record| record.execution == mcp::McpExecution::Provider)
            .map(|record| record.server_url))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use mcp::McpRegistryStore;

    use super::*;

    fn runtime_tool(
        name: &str,
        description: Option<String>,
        input_schema: serde_json::Value,
    ) -> RuntimeMcpTool {
        RuntimeMcpTool {
            remote_name: name.to_owned(),
            description,
            input_schema,
            output_schema: None,
            annotations: Some(serde_json::json!({"readOnlyHint": true})),
        }
    }

    struct CountingDiscoverer {
        calls: AtomicUsize,
        tools_list_changed: bool,
        ttl_ms: Option<u64>,
    }

    #[async_trait]
    impl McpToolDiscoverer for CountingDiscoverer {
        async fn discover_tools(
            &self,
            _server_url: &str,
            _auth: &McpRequestAuth,
            _trusted_universe: Option<uuid::Uuid>,
            _private_access: PrivateNetworkAccess,
            _limits: mcp::McpToolDiscoveryLimits,
        ) -> Result<DiscoveredMcpInventory, mcp::McpToolDiscoveryFailure> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok(DiscoveredMcpInventory {
                tools: vec![mcp::DiscoveredMcpTool {
                    name: format!("tool_{call}"),
                    title: None,
                    description: Some("test tool".to_owned()),
                    input_schema: serde_json::json!({"type": "object"}),
                    output_schema: Some(serde_json::json!({
                        "type": "object",
                        "properties": {"value": {"type": "string"}}
                    })),
                    annotations: None,
                }],
                tools_list_changed: self.tools_list_changed,
                ttl_ms: self.ttl_ms,
            })
        }
    }

    fn spec(revision: u64) -> RemoteMcpToolSpec {
        RemoteMcpToolSpec {
            server_id: "test".to_owned(),
            record_revision: revision,
            server_label: "test".to_owned(),
            description_ref: None,
            allowed_tools: None,
            execution: harness::RemoteMcpExecution::Runtime,
            exposure: harness::RemoteMcpExposure::Inject,
            approval: harness::RemoteMcpApprovalPolicy::Never,
            defer_loading: None,
        }
    }

    fn server_input(server_url: &str, allow_private_network: bool) -> mcp::PutMcpServerRecord {
        mcp::PutMcpServerRecord {
            server_id: mcp::McpServerId::new("test"),
            display_name: None,
            server_url: server_url.to_owned(),
            transport: mcp::RemoteMcpTransport::default(),
            default_server_label: "test".to_owned(),
            description: None,
            allowed_tools: None,
            execution: mcp::McpExecution::Runtime,
            exposure: mcp::McpExposure::Inject,
            approval: mcp::McpApprovalPolicy::Never,
            defer_loading: None,
            allow_private_network,
            auth_policy: mcp::McpServerAuthPolicy::None,
            auth_grant_id: None,
            headers: Default::default(),
            status: mcp::McpServerStatus::Active,
            now_ms: 1,
        }
    }

    /// A registry holding one usable record for the cache tests, which only look
    /// at what the discoverer returns.
    pub(super) struct OneServer;

    #[async_trait]
    impl mcp::McpRegistryStore for OneServer {
        async fn put_server(
            &self,
            _record: mcp::PutMcpServerRecord,
            _expected_revision: Option<u64>,
        ) -> Result<mcp::McpServerRecord, mcp::McpRegistryError> {
            unreachable!("the cache tests only read")
        }

        async fn read_server(
            &self,
            _server_id: &mcp::McpServerId,
        ) -> Result<mcp::McpServerRecord, mcp::McpRegistryError> {
            Ok(server_input("http://127.0.0.1/mcp", true).into_record())
        }

        async fn list_servers(
            &self,
            _request: mcp::ListMcpServers,
        ) -> Result<Vec<mcp::McpServerRecord>, mcp::McpRegistryError> {
            unreachable!("the cache tests only read")
        }

        async fn delete_server(
            &self,
            _server_id: &mcp::McpServerId,
        ) -> Result<mcp::McpServerRecord, mcp::McpRegistryError> {
            unreachable!("the cache tests only read")
        }
    }

    /// Records where each fetch went and what it was allowed to reach.
    #[derive(Default)]
    struct RecordingDiscoverer {
        fetches: std::sync::Mutex<Vec<(String, PrivateNetworkAccess)>>,
    }

    #[async_trait]
    impl McpToolDiscoverer for RecordingDiscoverer {
        async fn discover_tools(
            &self,
            server_url: &str,
            _auth: &McpRequestAuth,
            _trusted_universe: Option<uuid::Uuid>,
            private_access: PrivateNetworkAccess,
            _limits: mcp::McpToolDiscoveryLimits,
        ) -> Result<DiscoveredMcpInventory, mcp::McpToolDiscoveryFailure> {
            self.fetches
                .lock()
                .expect("fetches")
                .push((server_url.to_owned(), private_access));
            Ok(DiscoveredMcpInventory {
                tools: Vec::new(),
                tools_list_changed: false,
                ttl_ms: Some(0),
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn each_fetch_reads_the_url_and_network_opt_in_from_the_live_record() {
        let servers = Arc::new(mcp::InMemoryMcpRegistryStore::new());
        servers
            .put_server(server_input("http://127.0.0.1/first", true), None)
            .await
            .expect("record");
        let discoverer = Arc::new(RecordingDiscoverer::default());
        let resolver = RuntimeMcpInventoryResolver {
            servers: servers.clone(),
            ..RuntimeMcpInventoryResolver::for_test(
                discoverer.clone(),
                Duration::from_secs(60),
                Duration::from_secs(60),
            )
        };

        resolver.list_tools(&spec(1)).await.expect("first");
        servers
            .put_server(server_input("http://127.0.0.1/moved", false), None)
            .await
            .expect("edit");
        // The session's durable spec still names revision 1.
        resolver.list_tools(&spec(1)).await.expect("second");

        assert_eq!(
            *discoverer.fetches.lock().expect("fetches"),
            [
                (
                    "http://127.0.0.1/first".to_owned(),
                    PrivateNetworkAccess::Any
                ),
                (
                    "http://127.0.0.1/moved".to_owned(),
                    PrivateNetworkAccess::None
                ),
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn server_url_answers_only_for_a_usable_provider_hosted_record() {
        let servers = Arc::new(mcp::InMemoryMcpRegistryStore::new());
        let resolver = RuntimeMcpInventoryResolver {
            servers: servers.clone(),
            ..RuntimeMcpInventoryResolver::for_test(
                Arc::new(RecordingDiscoverer::default()),
                Duration::from_secs(60),
                Duration::from_secs(60),
            )
        };
        assert_eq!(resolver.server_url("test").await.expect("gone"), None);

        let hosted = || mcp::PutMcpServerRecord {
            execution: mcp::McpExecution::Provider,
            ..server_input("https://mcp.example.com/mcp", false)
        };
        servers.put_server(hosted(), None).await.expect("record");
        assert_eq!(
            resolver
                .server_url("test")
                .await
                .expect("usable")
                .as_deref(),
            Some("https://mcp.example.com/mcp")
        );

        let disabled = mcp::PutMcpServerRecord {
            status: mcp::McpServerStatus::Disabled,
            ..hosted()
        };
        servers.put_server(disabled, None).await.expect("disable");
        assert_eq!(resolver.server_url("test").await.expect("disabled"), None);

        // Moved to the runtime mid-run, it can name a private server: keep it from the provider.
        let runtime = server_input("http://10.0.0.1/private", true);
        servers.put_server(runtime, None).await.expect("runtime");
        assert_eq!(resolver.server_url("test").await.expect("runtime"), None);
    }

    /// A deleted or disabled server, or one moved to provider execution, becomes a
    /// failed call the model can read, and that text never holds the URL.
    #[tokio::test(flavor = "current_thread")]
    async fn a_runtime_call_against_an_unusable_record_fails_without_echoing_the_url() {
        let token_url = "https://mcp.example.com/s/PATH_SECRET/mcp?api_key=QUERY_SECRET";
        let servers = Arc::new(mcp::InMemoryMcpRegistryStore::new());
        let secrets: Arc<dyn SecretResolver> = Arc::new(llm_runtime::secrets::AbsentSecretResolver);
        let policy = McpPrivateNetworkPolicy::default();
        let runtime = RuntimeMcpExecutor::new(
            servers.clone(),
            secrets.clone(),
            Arc::new(RuntimeMcpInventoryResolver::new(
                servers.clone(),
                secrets,
                policy.clone(),
                ConfiguratorTrustedHeaderPolicy,
                uuid::Uuid::nil(),
            )),
            policy,
            ConfiguratorTrustedHeaderPolicy,
            uuid::Uuid::nil(),
        );
        let target = harness::RemoteMcpCallTarget {
            server_id: "test".to_owned(),
            record_revision: 1,
            server_label: "test".to_owned(),
            allowed_tools: None,
            approval: harness::RemoteMcpApprovalPolicy::Never,
        };

        let missing = runtime.runtime_record(&target).await.expect_err("missing");
        assert!(missing.contains("not available"), "{missing}");

        for (edit, expected) in [
            (
                mcp::PutMcpServerRecord {
                    status: mcp::McpServerStatus::Disabled,
                    ..server_input(token_url, false)
                },
                "not available",
            ),
            (
                mcp::PutMcpServerRecord {
                    execution: mcp::McpExecution::Provider,
                    ..server_input(token_url, false)
                },
                "no longer configured",
            ),
        ] {
            servers.put_server(edit, None).await.expect("edit");
            let error = runtime.runtime_record(&target).await.expect_err("refused");
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("SECRET"), "{error}");
        }

        // A changed URL needs no pin: the call uses the record as it stands.
        servers
            .put_server(server_input(token_url, false), None)
            .await
            .expect("edit");
        let record = runtime.runtime_record(&target).await.expect("usable");
        assert_eq!(record.server_url, token_url);
    }

    #[test]
    fn private_policy_trusts_a_listed_host_and_holds_the_rest_to_the_networks() {
        let policy = McpPrivateNetworkPolicy::parse(Some("MCP.internal,::1,10.20.0.0/16,fd00::/8"))
            .expect("policy");
        let networks = PrivateNetworkAccess::Networks(
            ["10.20.0.0/16", "fd00::/8"]
                .map(|network| network.parse().expect("network"))
                .into(),
        );

        // A listed host is matched as written, case aside, wherever it points.
        assert_eq!(
            policy.access("https://mcp.INTERNAL/mcp", true),
            PrivateNetworkAccess::Any
        );
        assert_eq!(
            policy.access("http://[::1]:8080/mcp", true),
            PrivateNetworkAccess::Any
        );
        // Every other host, name or literal, is judged by address at connect time.
        for url in [
            "https://mcp.tools.svc.cluster.local/mcp",
            "http://10.20.4.8/mcp",
            "http://10.21.4.8/mcp",
        ] {
            assert_eq!(policy.access(url, true), networks, "{url}");
        }
        // Without the record's opt-in the deployment list does not apply.
        for url in ["https://mcp.internal/mcp", "http://10.20.4.8/mcp"] {
            assert_eq!(policy.access(url, false), PrivateNetworkAccess::None);
        }
    }

    #[test]
    fn private_policy_without_networks_admits_only_listed_hosts() {
        let policy = McpPrivateNetworkPolicy::parse(Some("localhost")).expect("policy");
        assert_eq!(
            policy.access("http://localhost:3000/mcp", true),
            PrivateNetworkAccess::Any
        );
        assert_eq!(
            policy.access("https://mcp.tools.svc.cluster.local/mcp", true),
            PrivateNetworkAccess::None
        );
        assert_eq!(
            McpPrivateNetworkPolicy::default().access("http://10.20.4.8/mcp", true),
            PrivateNetworkAccess::None
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inventory_cache_singleflights_and_invalidates_on_revision() {
        let discoverer = Arc::new(CountingDiscoverer {
            calls: AtomicUsize::new(0),
            tools_list_changed: false,
            ttl_ms: None,
        });
        let resolver = Arc::new(RuntimeMcpInventoryResolver::for_test(
            discoverer.clone(),
            Duration::from_secs(60),
            Duration::from_secs(60),
        ));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let resolver = resolver.clone();
            tasks.push(tokio::spawn(async move {
                resolver.list_tools(&spec(1)).await.expect("inventory")
            }));
        }
        for task in tasks {
            assert_eq!(task.await.expect("join")[0].remote_name, "tool_1");
        }
        assert_eq!(discoverer.calls.load(Ordering::SeqCst), 1);

        resolver.list_tools(&spec(2)).await.expect("new revision");
        resolver
            .list_tools(&spec(1))
            .await
            .expect("old revision evicted");
        assert_eq!(discoverer.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inventory_cache_refreshes_after_ttl() {
        let discoverer = Arc::new(CountingDiscoverer {
            calls: AtomicUsize::new(0),
            tools_list_changed: false,
            ttl_ms: None,
        });
        let resolver = RuntimeMcpInventoryResolver::for_test(
            discoverer.clone(),
            Duration::from_millis(1),
            Duration::from_secs(60),
        );
        resolver.list_tools(&spec(1)).await.expect("first");
        tokio::time::sleep(Duration::from_millis(5)).await;
        resolver.list_tools(&spec(1)).await.expect("refreshed");
        assert_eq!(discoverer.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inventory_cache_uses_shorter_ttl_when_list_changes_are_advertised() {
        let discoverer = Arc::new(CountingDiscoverer {
            calls: AtomicUsize::new(0),
            tools_list_changed: true,
            ttl_ms: None,
        });
        let resolver = RuntimeMcpInventoryResolver::for_test(
            discoverer.clone(),
            Duration::from_secs(60),
            Duration::from_millis(1),
        );
        resolver.list_tools(&spec(1)).await.expect("first");
        tokio::time::sleep(Duration::from_millis(5)).await;
        resolver
            .list_tools(&spec(1))
            .await
            .expect("short TTL refresh");
        assert_eq!(discoverer.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inventory_cache_honors_advertised_ttl_before_fallbacks() {
        let discoverer = Arc::new(CountingDiscoverer {
            calls: AtomicUsize::new(0),
            tools_list_changed: true,
            ttl_ms: Some(60_000),
        });
        let resolver = RuntimeMcpInventoryResolver::for_test(
            discoverer.clone(),
            Duration::from_millis(1),
            Duration::from_millis(1),
        );
        resolver.list_tools(&spec(1)).await.expect("first");
        tokio::time::sleep(Duration::from_millis(5)).await;
        resolver
            .list_tools(&spec(1))
            .await
            .expect("server TTL cache hit");
        assert_eq!(discoverer.calls.load(Ordering::SeqCst), 1);
    }

    /// A discoverer whose server can go down, with tools `tool_a` and `tool_b`.
    struct FlakyDiscoverer {
        calls: AtomicUsize,
        failing: AtomicBool,
        delay: Duration,
    }

    impl FlakyDiscoverer {
        fn new(failing: bool, delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                failing: AtomicBool::new(failing),
                delay,
            })
        }
    }

    #[async_trait]
    impl McpToolDiscoverer for FlakyDiscoverer {
        async fn discover_tools(
            &self,
            _server_url: &str,
            _auth: &McpRequestAuth,
            _trusted_universe: Option<uuid::Uuid>,
            _private_access: PrivateNetworkAccess,
            _limits: mcp::McpToolDiscoveryLimits,
        ) -> Result<DiscoveredMcpInventory, mcp::McpToolDiscoveryFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            if self.failing.load(Ordering::SeqCst) {
                return Err(mcp::McpToolDiscoveryFailure::new(
                    mcp::McpToolDiscoveryFailureKind::Unreachable,
                    "MCP endpoint timed out",
                ));
            }
            let tool = |name: &str| mcp::DiscoveredMcpTool {
                name: name.to_owned(),
                title: None,
                description: None,
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: None,
                annotations: None,
            };
            Ok(DiscoveredMcpInventory {
                tools: vec![tool("tool_a"), tool("tool_b")],
                tools_list_changed: false,
                ttl_ms: None,
            })
        }
    }

    fn tool_names(tools: &[RuntimeMcpTool]) -> Vec<&str> {
        tools.iter().map(|tool| tool.remote_name.as_str()).collect()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_refresh_keeps_the_last_list_and_waits_before_asking_again() {
        let discoverer = FlakyDiscoverer::new(false, Duration::ZERO);
        let resolver = RuntimeMcpInventoryResolver::for_test(
            discoverer.clone(),
            Duration::from_millis(1),
            Duration::from_millis(60),
        );
        resolver.list_tools(&spec(1)).await.expect("first");
        tokio::time::sleep(Duration::from_millis(5)).await;

        discoverer.failing.store(true, Ordering::SeqCst);
        let kept = resolver.list_tools(&spec(1)).await.expect("kept");
        assert_eq!(tool_names(&kept), ["tool_a", "tool_b"]);
        assert_eq!(discoverer.calls.load(Ordering::SeqCst), 2);
        resolver.list_tools(&spec(1)).await.expect("inside window");
        assert_eq!(discoverer.calls.load(Ordering::SeqCst), 2);

        discoverer.failing.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(70)).await;
        resolver.list_tools(&spec(1)).await.expect("recovered");
        assert_eq!(discoverer.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_cold_worker_with_a_failing_server_gets_no_tools_and_no_error() {
        let discoverer = FlakyDiscoverer::new(true, Duration::ZERO);
        let resolver = RuntimeMcpInventoryResolver::for_test(
            discoverer.clone(),
            Duration::from_secs(60),
            Duration::from_millis(60),
        );
        let mut count = 0;
        let injected = llm_runtime::injected_runtime_tools(
            &resolver,
            &spec(1),
            &harness::ToolName::new("mcp_test"),
            &mut count,
        )
        .await
        .expect("a failing server never fails the turn");
        assert!(injected.is_empty());
        resolver.list_tools(&spec(1)).await.expect("inside window");
        assert_eq!(discoverer.calls.load(Ordering::SeqCst), 1);

        discoverer.failing.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(70)).await;
        let recovered = resolver.list_tools(&spec(1)).await.expect("recovered");
        assert_eq!(tool_names(&recovered), ["tool_a", "tool_b"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_turns_do_not_queue_fetches_behind_a_hanging_server() {
        let discoverer = FlakyDiscoverer::new(true, Duration::from_millis(50));
        let resolver = Arc::new(RuntimeMcpInventoryResolver::for_test(
            discoverer.clone(),
            Duration::from_secs(60),
            Duration::from_secs(60),
        ));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let resolver = resolver.clone();
            tasks.push(tokio::spawn(
                async move { resolver.list_tools(&spec(1)).await },
            ));
        }
        for task in tasks {
            assert!(task.await.expect("join").expect("no error").is_empty());
        }
        assert_eq!(discoverer.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn each_tool_subset_gets_its_own_list_through_a_failed_refresh() {
        let discoverer = FlakyDiscoverer::new(false, Duration::ZERO);
        let resolver = RuntimeMcpInventoryResolver::for_test(
            discoverer.clone(),
            Duration::from_millis(1),
            Duration::from_secs(60),
        );
        let subset = |tool: &str| RemoteMcpToolSpec {
            allowed_tools: Some(vec![tool.to_owned()]),
            ..spec(1)
        };
        let a = resolver.list_tools(&subset("tool_a")).await.expect("a");
        let b = resolver.list_tools(&subset("tool_b")).await.expect("b");
        assert_eq!(tool_names(&a), ["tool_a"]);
        assert_eq!(tool_names(&b), ["tool_b"]);

        tokio::time::sleep(Duration::from_millis(5)).await;
        discoverer.failing.store(true, Ordering::SeqCst);
        let a = resolver.list_tools(&subset("tool_a")).await.expect("a");
        let b = resolver.list_tools(&subset("tool_b")).await.expect("b");
        assert_eq!(tool_names(&a), ["tool_a"]);
        assert_eq!(tool_names(&b), ["tool_b"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn discovered_output_schema_reaches_cached_full_definitions() {
        let resolver = RuntimeMcpInventoryResolver::for_test(
            Arc::new(CountingDiscoverer {
                calls: AtomicUsize::new(0),
                tools_list_changed: false,
                ttl_ms: None,
            }),
            Duration::from_secs(60),
            Duration::from_secs(60),
        );
        let expected = serde_json::json!({
            "type": "object",
            "properties": {"value": {"type": "string"}}
        });
        for _ in 0..2 {
            let inventory = resolver.list_tools(&spec(1)).await.expect("inventory");
            assert_eq!(inventory[0].output_schema.as_ref(), Some(&expected));
            assert_eq!(
                full_tool_definition("test", &inventory[0])["outputSchema"],
                expected
            );
        }
    }

    #[test]
    fn binary_content_is_extracted_and_referenced_without_inline_base64() {
        let mut value = serde_json::json!({
            "content": [{"type": "image", "mimeType": "image/png", "data": "aGVsbG8="}]
        });
        let mut assets = Vec::new();
        extract_mcp_assets(&mut value, &mut assets).expect("extract");
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].bytes, b"hello");
        assert!(!value.to_string().contains("aGVsbG8="));
        attach_mcp_asset_refs(&mut value, &[harness::BlobRef::from_bytes(b"hello")]);
        assert!(value.to_string().contains("sha256:"));
    }

    /// The output-to-asset edges the tool activity records are exactly the
    /// refs the attached output embeds.
    #[test]
    fn attached_asset_refs_are_the_only_refs_the_output_embeds() {
        let mut value = serde_json::json!({
            "content": [
                {"type": "image", "mimeType": "image/png", "data": "aGVsbG8="},
                {"type": "text", "text": "see sha256:not-a-ref"},
                {"type": "resource", "resource": {"mimeType": "application/pdf", "blob": "d29ybGQ="}}
            ]
        });
        let mut assets = Vec::new();
        extract_mcp_assets(&mut value, &mut assets).expect("extract");
        let refs = assets
            .iter()
            .map(|asset| harness::BlobRef::from_bytes(&asset.bytes))
            .collect::<Vec<_>>();
        assert_eq!(refs.len(), 2);
        attach_mcp_asset_refs(&mut value, &refs);
        assert_eq!(
            harness::storage::collect_blob_refs(&value),
            refs.iter().cloned().collect()
        );
    }

    #[test]
    fn search_hits_are_uniform_and_oversized_descriptions_are_utf8_safe() {
        let whole = compact_search_hit(
            "docs",
            &runtime_tool(
                "read",
                Some("Read a document".to_owned()),
                serde_json::json!({
                    "type": "object",
                    "properties": {"page_id": {"type": "string"}}
                }),
            ),
        )
        .expect("whole hit");
        assert_eq!(
            whole["inputSchema"]["properties"]["page_id"]["type"],
            "string"
        );
        assert_eq!(whole["annotations"]["readOnlyHint"], true);
        assert!(whole.get("truncated").is_none());
        assert!(whole.get("outputSchema").is_none());

        let oversized = compact_search_hit(
            "docs",
            &runtime_tool(
                "long",
                Some("é".repeat(8_000)),
                serde_json::json!({"type": "object"}),
            ),
        )
        .expect("bounded hit");
        assert!(serialized_len(&oversized).unwrap() <= MCP_FIND_HIT_MAX_BYTES);
        assert_eq!(oversized["truncated"], MCP_FIND_TRUNCATED_NOTE);
        assert!(
            oversized["description"]
                .as_str()
                .unwrap()
                .is_char_boundary(oversized["description"].as_str().unwrap().len())
        );
    }

    #[test]
    fn deferred_definitions_and_search_hits_preserve_optional_output_schemas() {
        let mut tool = runtime_tool("read", None, serde_json::json!({"type": "object"}));
        assert!(
            full_tool_definition("docs", &tool)
                .get("outputSchema")
                .is_none()
        );
        assert!(
            compact_search_hit("docs", &tool)
                .unwrap()
                .get("outputSchema")
                .is_none()
        );

        let schema = serde_json::json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
            "additionalProperties": false
        });
        tool.output_schema = Some(schema.clone());
        assert_eq!(full_tool_definition("docs", &tool)["outputSchema"], schema);
        assert_eq!(
            compact_search_hit("docs", &tool).unwrap()["outputSchema"],
            schema
        );

        tool.description = Some("d".repeat(MCP_FIND_HIT_MAX_BYTES));
        let hit = compact_search_hit("docs", &tool).expect("bounded description");
        assert_eq!(hit["outputSchema"], schema);
        assert_eq!(hit["truncated"], MCP_FIND_TRUNCATED_NOTE);
        assert!(serialized_len(&hit).unwrap() <= MCP_FIND_HIT_MAX_BYTES);
    }

    #[test]
    fn oversized_output_schemas_are_omitted_only_from_compact_hits() {
        let mut tool = runtime_tool("read", None, serde_json::json!({"type": "object"}));
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"title": {"type": "string", "description": "d".repeat(MCP_FIND_HIT_MAX_BYTES)}}
        });
        tool.output_schema = Some(schema.clone());
        let hit = compact_search_hit("docs", &tool).expect("bounded schema");
        assert!(hit.get("outputSchema").is_none());
        assert_eq!(hit["truncated"], MCP_FIND_TRUNCATED_NOTE);
        assert!(serialized_len(&hit).unwrap() <= MCP_FIND_HIT_MAX_BYTES);
        assert_eq!(full_tool_definition("docs", &tool)["outputSchema"], schema);
    }

    #[test]
    fn oversized_schemas_fall_back_to_top_level_argument_names() {
        let hit = compact_search_hit(
            "docs",
            &runtime_tool(
                "update",
                Some("Update a page".to_owned()),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "page_id": {"type": "string", "description": "x".repeat(12_000)},
                        "title": {"type": "string"}
                    }
                }),
            ),
        )
        .expect("bounded hit");
        assert_eq!(hit["inputSchema"], serde_json::json!(["page_id", "title"]));
        assert_eq!(hit["truncated"], MCP_FIND_TRUNCATED_NOTE);
        assert!(serialized_len(&hit).unwrap() <= MCP_FIND_HIT_MAX_BYTES);
    }

    #[test]
    fn argument_validation_errors_include_the_full_input_schema() {
        let tool = runtime_tool(
            "update",
            None,
            serde_json::json!({
                "type": "object",
                "properties": {"page_id": {"type": "string"}},
                "required": ["page_id"]
            }),
        );
        let error = validate_mcp_arguments("update", &tool, &serde_json::Map::new())
            .expect_err("missing required argument");
        assert!(error.contains("input schema:"));
        assert!(error.contains(r#""required":["page_id"]"#));
    }

    #[test]
    fn search_pages_use_compact_byte_budget_and_advance_cursor() {
        let matches = (0..40)
            .map(|index| {
                (
                    None,
                    "docs".to_owned(),
                    runtime_tool(
                        &format!("tool_{index:02}"),
                        Some("d".repeat(2_000)),
                        serde_json::json!({"type": "object"}),
                    ),
                )
            })
            .collect::<Vec<_>>();
        let first = paged_tool_output(&matches, 0).expect("first page");
        assert!(serialized_len(&first).unwrap() <= MCP_FIND_PAGE_MAX_BYTES);
        let first_len = first["tools"].as_array().unwrap().len();
        let cursor = first["nextCursor"].as_u64().expect("next cursor") as usize;
        assert_eq!(cursor, first_len);
        assert!(cursor > 0);

        let second = paged_tool_output(&matches, cursor).expect("second page");
        assert!(serialized_len(&second).unwrap() <= MCP_FIND_PAGE_MAX_BYTES);
        assert_eq!(second["tools"][0]["name"], format!("tool_{cursor:02}"));
        assert!(second["nextCursor"].is_null());
    }

    fn png_bytes() -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0u8; 1024]);
        bytes
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn runtime_result(
        content: serde_json::Value,
    ) -> (String, Vec<RuntimeMcpMedia>, Vec<RuntimeMcpAsset>) {
        let raw = serde_json::json!({ "content": content });
        let mut output = serde_json::json!({ "content": raw["content"].clone() });
        let mut assets = Vec::new();
        extract_mcp_assets(output.get_mut("content").expect("content"), &mut assets)
            .expect("extract");
        let (visible, media) = visible_mcp_result(&raw, &output, &assets);
        (visible, media, assets)
    }

    #[test]
    fn visible_text_announces_admitted_media_by_position_and_handle() {
        let png = png_bytes();
        let jpeg = b"\xff\xd8\xff\xe0 second".to_vec();
        let pdf = b"%PDF-1.7 report".to_vec();
        let (visible, media, assets) = runtime_result(serde_json::json!([
            {"type": "text", "text": "Row 1 rendered."},
            {"type": "image", "mimeType": "image/png", "data": b64(&png)},
            {"type": "image", "mimeType": "image/jpeg", "data": b64(&jpeg)},
            {"type": "resource", "resource": {
                "uri": "imaginator://assets/qyk567/report.pdf",
                "mimeType": "application/pdf", "blob": b64(&pdf)
            }},
            {"type": "resource", "resource": {"uri": "imaginator://models", "mimeType": "application/json", "text": "{}"}},
            {"type": "resource_link", "uri": "https://example.test/full.png", "mimeType": "image/png"}
        ]));
        let lines = visible.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "Row 1 rendered.");
        assert_eq!(
            lines[1],
            format!(
                "[image 1 · {} · image/png · 2 KiB]",
                harness::media::media_handle(&harness::BlobRef::from_bytes(&png))
            )
        );
        assert!(
            lines[2].starts_with("[image 2 · media:") && lines[2].ends_with("· image/jpeg · 11 B]"),
            "{}",
            lines[2]
        );
        assert!(
            lines[3].starts_with("[document 1 · media:")
                && lines[3].contains("· application/pdf · report.pdf ·"),
            "{}",
            lines[3]
        );
        assert_eq!(lines[4], "[resource: imaginator://models]");
        assert_eq!(lines[5], "{}");
        assert_eq!(
            lines[6],
            "[resource_link: https://example.test/full.png · image/png]"
        );
        assert_eq!(
            media,
            vec![
                RuntimeMcpMedia {
                    asset_index: 0,
                    kind: harness::media::MediaKind::Image,
                    media_type: "image/png".into(),
                    name: None
                },
                RuntimeMcpMedia {
                    asset_index: 1,
                    kind: harness::media::MediaKind::Image,
                    media_type: "image/jpeg".into(),
                    name: None
                },
                RuntimeMcpMedia {
                    asset_index: 2,
                    kind: harness::media::MediaKind::Document,
                    media_type: "application/pdf".into(),
                    name: Some("report.pdf".into())
                },
            ]
        );
        assert_eq!(assets.len(), 3);
        assert_eq!(
            assets[2].uri.as_deref(),
            Some("imaginator://assets/qyk567/report.pdf")
        );
    }

    #[test]
    fn unsupported_oversized_and_excess_media_are_dropped_with_notes_not_errors() {
        let mut content = vec![
            serde_json::json!({"type": "image", "mimeType": "image/svg+xml", "data": b64(b"<svg/>")}),
            serde_json::json!({"type": "audio", "mimeType": "audio/wav", "data": b64(b"RIFF")}),
            serde_json::json!({"type": "image", "mimeType": "image/png", "data": b64(&vec![0u8; (harness::media::MAX_TOOL_MEDIA_BYTES + 1) as usize])}),
        ];
        for index in 0..9u8 {
            content.push(serde_json::json!({"type": "image", "mimeType": "image/png", "data": b64(&[index; 16])}));
        }
        let (visible, media, _) = runtime_result(serde_json::Value::Array(content));
        let lines = visible.lines().collect::<Vec<_>>();
        assert_eq!(
            lines[0],
            "[image 1 omitted: image/svg+xml is not supported]"
        );
        assert_eq!(lines[1], "[audio 1 omitted: audio/wav is not supported]");
        assert_eq!(
            lines[2],
            format!(
                "[image 2 omitted: {} bytes exceeds the {} byte limit]",
                harness::media::MAX_TOOL_MEDIA_BYTES + 1,
                harness::media::MAX_TOOL_MEDIA_BYTES
            )
        );
        assert_eq!(media.len(), harness::media::MAX_TOOL_MEDIA_ITEMS);
        assert!(lines[3].starts_with("[image 3 · media:"));
        assert_eq!(
            lines[11],
            "[image 11 omitted: at most 8 media items per result]"
        );
        assert_eq!(lines.len(), 12);
    }

    #[test]
    fn mixed_media_shares_the_cap_and_preserves_asset_indices_and_rejection_precedence() {
        let mut content = vec![
            serde_json::json!({"type": "image", "mimeType": "image/svg+xml", "data": b64(b"svg")}),
            // Audio blocks remain unsupported even if they claim an image MIME.
            serde_json::json!({"type": "audio", "mimeType": "image/png", "data": b64(b"audio")}),
        ];
        let mut expected_media = Vec::new();
        for index in 0..harness::media::MAX_TOOL_MEDIA_ITEMS {
            let (kind, media_type, name) = if index % 2 == 0 {
                content.push(serde_json::json!({
                    "type": "image", "mimeType": "Image/PNG; charset=binary", "data": b64(b"png")
                }));
                (harness::media::MediaKind::Image, "image/png", None)
            } else {
                let name = format!("report-{index}.pdf");
                content.push(serde_json::json!({"type": "resource", "resource": {
                    "uri": format!("docs://reports/{name}"),
                    "mimeType": "Application/PDF", "blob": b64(b"pdf")
                }}));
                (
                    harness::media::MediaKind::Document,
                    "application/pdf",
                    Some(name),
                )
            };
            expected_media.push(RuntimeMcpMedia {
                asset_index: index + 2,
                kind,
                media_type: media_type.to_owned(),
                name,
            });
        }
        content.extend([
            serde_json::json!({"type": "text", "text": "between blocks"}),
            serde_json::json!({"type": "image", "mimeType": "image/png", "data": b64(b"excess")}),
            serde_json::json!({"type": "resource", "resource": {
                "uri": "docs://excess.pdf", "mimeType": "application/pdf", "blob": b64(b"excess")
            }}),
            serde_json::json!({"type": "resource", "resource": {
                "mimeType": "image/svg+xml", "blob": b64(b"svg")
            }}),
        ]);
        let (visible, media, assets) = runtime_result(serde_json::Value::Array(content));
        assert_eq!(media, expected_media);
        assert_eq!(assets.len(), harness::media::MAX_TOOL_MEDIA_ITEMS + 5);
        let lines: Vec<_> = visible.lines().collect();
        assert_eq!(
            lines[0],
            "[image 1 omitted: image/svg+xml is not supported]"
        );
        assert_eq!(lines[1], "[audio 1 omitted: image/png is not supported]");
        for (index, item) in media.iter().enumerate() {
            let position = if index % 2 == 0 {
                index / 2 + 2
            } else {
                index / 2 + 1
            };
            let label = if index % 2 == 0 { "image" } else { "document" };
            let handle = harness::media::media_handle(&assets[item.asset_index].blob_ref);
            let name = item
                .name
                .as_ref()
                .map(|name| format!(" · {name}"))
                .unwrap_or_default();
            assert_eq!(
                lines[index + 2],
                format!(
                    "[{label} {position} · {handle} · {}{name} · 3 B]",
                    item.media_type
                )
            );
        }
        assert_eq!(
            &lines[harness::media::MAX_TOOL_MEDIA_ITEMS + 2..],
            &[
                "between blocks",
                "[image 6 omitted: at most 8 media items per result]",
                "[document 5 omitted: at most 8 media items per result]",
                "[document 6 omitted: image/svg+xml is not supported]",
            ]
        );
    }

    #[test]
    fn media_entries_are_built_from_admitted_assets_in_order() {
        let (_, media, assets) = runtime_result(serde_json::json!([
            {"type": "image", "mimeType": "image/png", "data": b64(&png_bytes())}
        ]));
        let item = &media[0];
        let entry = harness::media::media_context_entry(
            assets[item.asset_index].blob_ref.clone(),
            &item.media_type,
            item.kind,
            item.name.as_deref(),
        );
        assert_eq!(entry.content.media_type.as_deref(), Some("image/png"));
        assert_eq!(entry.preview.as_deref(), Some("[image]"));
        assert!(matches!(
            entry.kind,
            harness::ContextEntryKind::Message {
                role: harness::ContextMessageRole::User
            }
        ));
    }
}
