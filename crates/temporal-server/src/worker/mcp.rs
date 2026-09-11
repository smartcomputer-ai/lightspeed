use std::{
    collections::HashMap,
    net::IpAddr,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use auth::SecretValue;
use base64::Engine;
use engine::RemoteMcpToolSpec;
use ipnet::IpNet;
use llm_runtime::{
    McpInventoryError, McpInventoryResolver, NativeMcpTool,
    secrets::{SecretResolveError, SecretResolver},
};
use mcp::search::McpToolSearchQuery;
use tokio::sync::Mutex;
use url::Url;

use crate::gateway::service::mcp_discovery::{
    ConfiguratorTrustedHeaderPolicy, DiscoveredMcpInventory, HttpMcpToolDiscoverer,
    McpToolCallRequest, McpToolDiscoverer,
};

pub enum NativeMcpExecutionOutcome {
    Completed {
        output: serde_json::Value,
        visible: String,
        is_error: bool,
        assets: Vec<NativeMcpAsset>,
        /// Admitted `content` assets, in content order, that become media
        /// entries after the tool result. Indexes point into `assets`.
        media: Vec<NativeMcpMedia>,
    },
    NeedsApproval {
        subject: engine::ApprovalSubject,
    },
}

/// Binary content stripped out of a tool result: `image` and `audio` blocks
/// and embedded `resource` blobs, from `content` first and then from
/// `structuredContent`, in document order.
pub struct NativeMcpAsset {
    pub bytes: Vec<u8>,
    /// Content identity of `bytes`; the store returns the same reference.
    pub blob_ref: engine::BlobRef,
    pub media_type: Option<String>,
    pub kind: String,
    /// Resource `uri`, when the asset came from an embedded resource.
    pub uri: Option<String>,
}

/// One admitted asset the model gets to see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeMcpMedia {
    pub asset_index: usize,
    pub kind: engine::media::MediaKind,
    pub media_type: String,
    pub name: Option<String>,
}

#[derive(Clone)]
pub struct NativeMcpRuntime {
    servers: Arc<dyn mcp::McpRegistryStore>,
    secrets: Arc<dyn SecretResolver>,
    inventory: Arc<NativeMcpInventoryResolver>,
    transport: HttpMcpToolDiscoverer,
    private_networks: McpPrivateNetworkPolicy,
    trusted_header: ConfiguratorTrustedHeaderPolicy,
    universe_id: uuid::Uuid,
}

/// Demand-driven inventory TTL for servers without a tool-list change signal.
pub const MCP_INVENTORY_CACHE_TTL: Duration = Duration::from_secs(300);
/// Short fallback while Lightspeed does not maintain notification subscriptions.
pub const MCP_LIST_CHANGED_CACHE_TTL: Duration = Duration::from_secs(60);
const MCP_FIND_HIT_MAX_BYTES: usize = 8 * 1024;
const MCP_FIND_PAGE_MAX_BYTES: usize = 64 * 1024;
const MCP_FIND_DETAIL_MAX_NAMES: usize = 5;
const MCP_FIND_TRUNCATED_NOTE: &str =
    "Call mcp_find_tools with server and names for the full definition.";

type InventoryCacheKey = (String, u64);
type InventoryLocks = HashMap<InventoryCacheKey, Arc<Mutex<()>>>;

#[derive(Clone, Debug, Default)]
pub struct McpPrivateNetworkPolicy {
    hosts: Arc<Vec<String>>,
    networks: Arc<Vec<IpNet>>,
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
            networks: Arc::new(networks),
        })
    }

    pub fn permits(&self, server_url: &str, record_opt_in: bool) -> bool {
        if !record_opt_in {
            return false;
        }
        let Ok(url) = Url::parse(server_url) else {
            return false;
        };
        let Some(host) = url.host_str() else {
            return false;
        };
        if self
            .hosts
            .iter()
            .any(|allowed| allowed == &host.to_ascii_lowercase())
        {
            return true;
        }
        host.parse::<IpAddr>().is_ok_and(|address| {
            self.networks
                .iter()
                .any(|network| network.contains(&address))
        })
    }
}

#[derive(Clone)]
pub struct NativeMcpInventoryResolver {
    discoverer: Arc<dyn McpToolDiscoverer>,
    secrets: Arc<dyn SecretResolver>,
    private_networks: McpPrivateNetworkPolicy,
    trusted_header: ConfiguratorTrustedHeaderPolicy,
    universe_id: uuid::Uuid,
    cache: Arc<Mutex<HashMap<InventoryCacheKey, CachedInventory>>>,
    locks: Arc<Mutex<InventoryLocks>>,
    ttl: Duration,
    list_changed_ttl: Duration,
}

#[derive(Clone)]
struct CachedInventory {
    fetched_at: Instant,
    ttl: Duration,
    tools: Vec<NativeMcpTool>,
}

impl NativeMcpInventoryResolver {
    pub(crate) fn new(
        secrets: Arc<dyn SecretResolver>,
        private_networks: McpPrivateNetworkPolicy,
        trusted_header: ConfiguratorTrustedHeaderPolicy,
        universe_id: uuid::Uuid,
    ) -> Self {
        Self {
            discoverer: Arc::new(HttpMcpToolDiscoverer::new()),
            secrets,
            private_networks,
            trusted_header,
            universe_id,
            cache: Arc::new(Mutex::new(HashMap::new())),
            locks: Arc::new(Mutex::new(HashMap::new())),
            ttl: MCP_INVENTORY_CACHE_TTL,
            list_changed_ttl: MCP_LIST_CHANGED_CACHE_TTL,
        }
    }

    #[cfg(test)]
    fn for_test(
        discoverer: Arc<dyn McpToolDiscoverer>,
        ttl: Duration,
        list_changed_ttl: Duration,
    ) -> Self {
        Self {
            discoverer,
            secrets: Arc::new(llm_runtime::secrets::AbsentSecretResolver),
            private_networks: McpPrivateNetworkPolicy::parse(Some("127.0.0.1"))
                .expect("test private policy"),
            trusted_header: ConfiguratorTrustedHeaderPolicy::default(),
            universe_id: uuid::Uuid::nil(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            locks: Arc::new(Mutex::new(HashMap::new())),
            ttl,
            list_changed_ttl,
        }
    }

    async fn bearer(
        &self,
        spec: &RemoteMcpToolSpec,
    ) -> Result<Option<SecretValue>, McpInventoryError> {
        let Some(secret_ref) = spec.auth_ref.as_ref() else {
            return Ok(None);
        };
        match self
            .secrets
            .resolve(secret_ref, Some(spec.server_url.as_str()))
            .await
        {
            Ok(value) => Ok(Some(SecretValue::new(value.expose()))),
            Err(SecretResolveError::CredentialAbsent { .. }) if !spec.auth_required => Ok(None),
            Err(error) => Err(McpInventoryError::new(format!(
                "resolve MCP credential: {error}"
            ))),
        }
    }

    async fn uncached(
        &self,
        spec: &RemoteMcpToolSpec,
    ) -> Result<(Vec<NativeMcpTool>, bool, Option<Duration>), McpInventoryError> {
        let trusted_universe = self
            .trusted_header
            .permits(&spec.server_id, &spec.server_url)
            .then_some(self.universe_id);
        let bearer = if trusted_universe.is_some() {
            None
        } else {
            self.bearer(spec).await?
        };
        let allow_private = self
            .private_networks
            .permits(&spec.server_url, spec.allow_private_network);
        let discovered = self
            .discoverer
            .discover_tools(
                &spec.server_url,
                bearer.as_ref(),
                trusted_universe,
                allow_private,
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
            .map(|tool| NativeMcpTool {
                remote_name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
                annotations: tool.annotations.map(native_tool_annotations),
            })
            .collect::<Vec<_>>();
        tools.sort_by(|left, right| left.remote_name.cmp(&right.remote_name));
        Ok((tools, tools_list_changed, ttl_ms.map(Duration::from_millis)))
    }
}

impl NativeMcpRuntime {
    pub(crate) fn new(
        servers: Arc<dyn mcp::McpRegistryStore>,
        secrets: Arc<dyn SecretResolver>,
        inventory: Arc<NativeMcpInventoryResolver>,
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
        request: &engine::ToolInvocationCallRequest,
        arguments: serde_json::Value,
    ) -> Result<NativeMcpExecutionOutcome, String> {
        let runtime = request
            .call
            .remote_mcp
            .as_ref()
            .ok_or_else(|| "native MCP runtime facts are missing".to_owned())?;
        match runtime {
            engine::RemoteMcpCallRuntime::Injected {
                target,
                remote_tool_name,
                approval_decision,
            } => {
                let arguments = arguments
                    .as_object()
                    .cloned()
                    .ok_or_else(|| "native MCP tool arguments must be a JSON object".to_owned())?;
                self.call(
                    request,
                    target,
                    remote_tool_name,
                    arguments,
                    *approval_decision,
                )
                .await
            }
            engine::RemoteMcpCallRuntime::Search {
                targets,
                approval_decision,
            } if request
                .call
                .tool_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "mcp.find_tools") =>
            {
                self.find_tools(targets, arguments).await
            }
            engine::RemoteMcpCallRuntime::Search {
                targets,
                approval_decision,
            } if request
                .call
                .tool_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "mcp.call") =>
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
            engine::RemoteMcpCallRuntime::Search { .. } => {
                Err("unknown native MCP search meta-tool".to_owned())
            }
        }
    }

    async fn find_tools(
        &self,
        targets: &[engine::RemoteMcpCallTarget],
        arguments: serde_json::Value,
    ) -> Result<NativeMcpExecutionOutcome, String> {
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
            let spec = spec_from_target(target, engine::RemoteMcpExposure::Search);
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
            return Ok(NativeMcpExecutionOutcome::Completed {
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
            let spec = spec_from_target(target, engine::RemoteMcpExposure::Search);
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
        Ok(NativeMcpExecutionOutcome::Completed {
            visible: serde_json::to_string(&output).map_err(|error| error.to_string())?,
            output,
            is_error: false,
            assets: Vec::new(),
            media: Vec::new(),
        })
    }

    async fn validate_search_call(
        &self,
        target: &engine::RemoteMcpCallTarget,
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
        let spec = spec_from_target(target, engine::RemoteMcpExposure::Search);
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
        request: &engine::ToolInvocationCallRequest,
        target: &engine::RemoteMcpCallTarget,
        tool_name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
        approval_decision: Option<bool>,
    ) -> Result<NativeMcpExecutionOutcome, String> {
        if target.approval == engine::RemoteMcpApprovalPolicy::Always {
            match approval_decision {
                None => {
                    return Ok(NativeMcpExecutionOutcome::NeedsApproval {
                        subject: engine::ApprovalSubject::McpToolCall {
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
                    return Ok(NativeMcpExecutionOutcome::Completed {
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
        self.validate_current_record(target).await?;
        let trusted_universe = self
            .trusted_header
            .permits(&target.server_id, &target.server_url)
            .then_some(self.universe_id);
        let bearer = if trusted_universe.is_some() {
            None
        } else {
            self.bearer_for_target(target).await?
        };
        let allow_private = self
            .private_networks
            .permits(&target.server_url, target.allow_private_network);
        let result = self
            .transport
            .call_tool(
                &target.server_url,
                bearer.as_ref(),
                trusted_universe,
                allow_private,
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
                .expect("native MCP output object")
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
        Ok(NativeMcpExecutionOutcome::Completed {
            output,
            visible,
            is_error: result.is_error.unwrap_or(false),
            assets,
            media,
        })
    }

    async fn validate_current_record(
        &self,
        target: &engine::RemoteMcpCallTarget,
    ) -> Result<(), String> {
        let id = mcp::McpServerId::try_new(target.server_id.clone())
            .map_err(|error| format!("invalid MCP server id: {error}"))?;
        let record = self
            .servers
            .read_server(&id)
            .await
            .map_err(|error| error.to_string())?;
        if record.server_url != target.server_url {
            return Err(format!(
                "MCP server URL changed from admitted audience {} to {}",
                target.server_url, record.server_url
            ));
        }
        if record.status != mcp::McpServerStatus::Active
            && record.status != mcp::McpServerStatus::Unverified
        {
            return Err(format!("MCP server {} is not active", target.server_id));
        }
        if record.execution != mcp::McpExecution::Native {
            return Err(format!(
                "MCP server {} is no longer configured for native execution",
                target.server_id
            ));
        }
        Ok(())
    }

    async fn bearer_for_target(
        &self,
        target: &engine::RemoteMcpCallTarget,
    ) -> Result<Option<SecretValue>, String> {
        let Some(secret_ref) = target.auth_ref.as_ref() else {
            return Ok(None);
        };
        match self
            .secrets
            .resolve(secret_ref, Some(&target.server_url))
            .await
        {
            Ok(value) => Ok(Some(SecretValue::new(value.expose()))),
            Err(SecretResolveError::CredentialAbsent { .. }) if !target.auth_required => Ok(None),
            Err(error) => Err(format!("resolve MCP credential: {error}")),
        }
    }
}

fn native_tool_annotations(annotations: mcp::McpToolAnnotations) -> serde_json::Value {
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
    tool: &NativeMcpTool,
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

fn full_tool_definition(server: &str, tool: &NativeMcpTool) -> serde_json::Value {
    serde_json::json!({
        "server": server,
        "name": tool.remote_name,
        "description": tool.description,
        "inputSchema": tool.input_schema,
        "annotations": tool.annotations,
    })
}

fn compact_search_hit(server: &str, tool: &NativeMcpTool) -> Result<serde_json::Value, String> {
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
        NativeMcpTool,
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
    assets: &mut Vec<NativeMcpAsset>,
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
                assets.push(NativeMcpAsset {
                    blob_ref: engine::BlobRef::from_bytes(&bytes),
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
                assets.push(NativeMcpAsset {
                    blob_ref: engine::BlobRef::from_bytes(&bytes),
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

pub fn attach_mcp_asset_refs(value: &mut serde_json::Value, refs: &[engine::BlobRef]) {
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
    target: &engine::RemoteMcpCallTarget,
    exposure: engine::RemoteMcpExposure,
) -> RemoteMcpToolSpec {
    RemoteMcpToolSpec {
        server_id: target.server_id.clone(),
        record_revision: target.record_revision,
        server_label: target.server_label.clone(),
        server_url: target.server_url.clone(),
        description_ref: None,
        allowed_tools: target.allowed_tools.clone(),
        execution: engine::RemoteMcpExecution::Native,
        exposure,
        approval: target.approval.clone(),
        defer_loading: None,
        auth_ref: target.auth_ref.clone(),
        auth_required: target.auth_required,
        allow_private_network: target.allow_private_network,
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
    assets: &[NativeMcpAsset],
) -> (String, Vec<NativeMcpMedia>) {
    use engine::media::{
        MediaRejection, admit_tool_media, tool_media_line, tool_media_omitted_line,
    };

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
                    if media.len() >= engine::media::MAX_TOOL_MEDIA_ITEMS {
                        Err(MediaRejection::TooMany {
                            limit: engine::media::MAX_TOOL_MEDIA_ITEMS,
                        })
                    } else {
                        Ok(kind)
                    }
                });
                match admission {
                    Ok(kind) => {
                        let media_type = engine::media::normalized_media_type(
                            asset.media_type.as_deref().unwrap_or_default(),
                        );
                        lines.push(tool_media_line(
                            kind,
                            index,
                            &asset.blob_ref,
                            &media_type,
                            None,
                            asset.bytes.len() as u64,
                        ));
                        media.push(NativeMcpMedia {
                            asset_index,
                            kind,
                            media_type,
                            name: None,
                        });
                    }
                    Err(rejection) => {
                        lines.push(tool_media_omitted_line(label, index, &rejection));
                    }
                }
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
                let admission =
                    admit_tool_media(asset.media_type.as_deref(), asset.bytes.len() as u64)
                        .and_then(|kind| {
                            if media.len() >= engine::media::MAX_TOOL_MEDIA_ITEMS {
                                Err(MediaRejection::TooMany {
                                    limit: engine::media::MAX_TOOL_MEDIA_ITEMS,
                                })
                            } else {
                                Ok(kind)
                            }
                        });
                match admission {
                    Ok(kind) => {
                        let media_type = engine::media::normalized_media_type(
                            asset.media_type.as_deref().unwrap_or_default(),
                        );
                        lines.push(tool_media_line(
                            kind,
                            index,
                            &asset.blob_ref,
                            &media_type,
                            name.as_deref(),
                            asset.bytes.len() as u64,
                        ));
                        media.push(NativeMcpMedia {
                            asset_index,
                            kind,
                            media_type,
                            name,
                        });
                    }
                    Err(rejection) => {
                        lines.push(tool_media_omitted_line("document", index, &rejection));
                    }
                }
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
impl McpInventoryResolver for NativeMcpInventoryResolver {
    async fn list_tools(
        &self,
        spec: &RemoteMcpToolSpec,
    ) -> Result<Vec<NativeMcpTool>, McpInventoryError> {
        let key = (spec.server_id.clone(), spec.record_revision);
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
        let (tools, tools_list_changed, advertised_ttl) = self.uncached(spec).await?;
        let ttl = advertised_ttl.unwrap_or(if tools_list_changed {
            self.list_changed_ttl
        } else {
            self.ttl
        });
        let mut cache = self.cache.lock().await;
        cache.retain(|(server_id, revision), _| {
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
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn native_tool(
        name: &str,
        description: Option<String>,
        input_schema: serde_json::Value,
    ) -> NativeMcpTool {
        NativeMcpTool {
            remote_name: name.to_owned(),
            description,
            input_schema,
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
            _bearer: Option<&SecretValue>,
            _trusted_universe: Option<uuid::Uuid>,
            _allow_private_network: bool,
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
            server_url: "http://127.0.0.1/mcp".to_owned(),
            description_ref: None,
            allowed_tools: None,
            execution: engine::RemoteMcpExecution::Native,
            exposure: engine::RemoteMcpExposure::Inject,
            approval: engine::RemoteMcpApprovalPolicy::Never,
            defer_loading: None,
            auth_ref: None,
            auth_required: false,
            allow_private_network: true,
        }
    }

    #[test]
    fn private_policy_requires_record_opt_in_and_matching_host_or_cidr() {
        let policy = McpPrivateNetworkPolicy::parse(Some("mcp.internal,10.20.0.0/16,fd00::/8"))
            .expect("policy");
        assert!(policy.permits("https://mcp.internal/mcp", true));
        assert!(policy.permits("http://10.20.4.8/mcp", true));
        assert!(!policy.permits("http://10.20.4.8/mcp", false));
        assert!(!policy.permits("http://10.21.4.8/mcp", true));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inventory_cache_singleflights_and_invalidates_on_revision() {
        let discoverer = Arc::new(CountingDiscoverer {
            calls: AtomicUsize::new(0),
            tools_list_changed: false,
            ttl_ms: None,
        });
        let resolver = Arc::new(NativeMcpInventoryResolver::for_test(
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
        let resolver = NativeMcpInventoryResolver::for_test(
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
        let resolver = NativeMcpInventoryResolver::for_test(
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
        let resolver = NativeMcpInventoryResolver::for_test(
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
        attach_mcp_asset_refs(&mut value, &[engine::BlobRef::from_bytes(b"hello")]);
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
            .map(|asset| engine::BlobRef::from_bytes(&asset.bytes))
            .collect::<Vec<_>>();
        assert_eq!(refs.len(), 2);
        attach_mcp_asset_refs(&mut value, &refs);
        assert_eq!(
            engine::storage::collect_blob_refs(&value),
            refs.iter().cloned().collect()
        );
    }

    #[test]
    fn search_hits_are_uniform_and_oversized_descriptions_are_utf8_safe() {
        let whole = compact_search_hit(
            "docs",
            &native_tool(
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

        let oversized = compact_search_hit(
            "docs",
            &native_tool(
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
    fn oversized_schemas_fall_back_to_top_level_argument_names() {
        let hit = compact_search_hit(
            "docs",
            &native_tool(
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
        let tool = native_tool(
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
                    native_tool(
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

    fn native_result(
        content: serde_json::Value,
    ) -> (String, Vec<NativeMcpMedia>, Vec<NativeMcpAsset>) {
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
        let (visible, media, assets) = native_result(serde_json::json!([
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
                engine::media::media_handle(&engine::BlobRef::from_bytes(&png))
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
                NativeMcpMedia {
                    asset_index: 0,
                    kind: engine::media::MediaKind::Image,
                    media_type: "image/png".into(),
                    name: None
                },
                NativeMcpMedia {
                    asset_index: 1,
                    kind: engine::media::MediaKind::Image,
                    media_type: "image/jpeg".into(),
                    name: None
                },
                NativeMcpMedia {
                    asset_index: 2,
                    kind: engine::media::MediaKind::Document,
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
            serde_json::json!({"type": "image", "mimeType": "image/png", "data": b64(&vec![0u8; (engine::media::MAX_TOOL_MEDIA_BYTES + 1) as usize])}),
        ];
        for index in 0..9u8 {
            content.push(serde_json::json!({"type": "image", "mimeType": "image/png", "data": b64(&[index; 16])}));
        }
        let (visible, media, _) = native_result(serde_json::Value::Array(content));
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
                engine::media::MAX_TOOL_MEDIA_BYTES + 1,
                engine::media::MAX_TOOL_MEDIA_BYTES
            )
        );
        assert_eq!(media.len(), engine::media::MAX_TOOL_MEDIA_ITEMS);
        assert!(lines[3].starts_with("[image 3 · media:"));
        assert_eq!(
            lines[11],
            "[image 11 omitted: at most 8 media items per result]"
        );
        assert_eq!(lines.len(), 12);
    }

    #[test]
    fn media_entries_are_built_from_admitted_assets_in_order() {
        let (_, media, assets) = native_result(serde_json::json!([
            {"type": "image", "mimeType": "image/png", "data": b64(&png_bytes())}
        ]));
        let item = &media[0];
        let entry = engine::media::media_context_entry(
            assets[item.asset_index].blob_ref.clone(),
            &item.media_type,
            item.kind,
            item.name.as_deref(),
        );
        assert_eq!(entry.content.media_type.as_deref(), Some("image/png"));
        assert_eq!(entry.preview.as_deref(), Some("[image]"));
        assert!(matches!(
            entry.kind,
            engine::ContextEntryKind::Message {
                role: engine::ContextMessageRole::User
            }
        ));
    }
}
