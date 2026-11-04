use async_trait::async_trait;
use harness::{RemoteMcpToolSpec, ToolName};
use serde_json::Value;

use crate::{LlmAdapterError, LlmAdapterResult};

pub const MAX_RUNTIME_MCP_TOOLS_PER_REQUEST: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeMcpTool {
    pub remote_name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    /// Optional schema of `structuredContent`; the MCP result envelope and
    /// content blocks are outside this schema.
    pub output_schema: Option<Value>,
    /// Standard MCP annotation hints retained for model-facing discovery.
    /// They are untrusted metadata and never authorize execution or retries.
    pub annotations: Option<Value>,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct McpInventoryError {
    pub message: String,
    /// The registry behind this answer may recover, so the attempt is worth retrying.
    pub transient: bool,
}

impl McpInventoryError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: false,
        }
    }

    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            transient: true,
        }
    }
}

/// What the adapters need to know about an MCP server beyond its durable spec,
/// read from the server's live record so an admin's change applies at once.
#[async_trait]
pub trait McpInventoryResolver: Send + Sync {
    async fn list_tools(
        &self,
        spec: &RemoteMcpToolSpec,
    ) -> Result<Vec<RuntimeMcpTool>, McpInventoryError>;

    /// The URL a provider should connect to, or `None` when the server's record
    /// is gone, switched off or no longer provider-hosted.
    async fn server_url(&self, server_id: &str) -> Result<Option<String>, McpInventoryError>;
}

/// The URL for a provider-hosted server in this request. A server with no usable
/// record is left out, so the turn runs without its tools.
pub(crate) async fn provider_hosted_server_url(
    inventory: &dyn McpInventoryResolver,
    spec: &RemoteMcpToolSpec,
) -> LlmAdapterResult<Option<String>> {
    let server_url = inventory
        .server_url(&spec.server_id)
        .await
        .map_err(|error| LlmAdapterError::McpInventory {
            server: spec.server_id.clone(),
            message: error.to_string(),
            transient: error.transient,
        })?;
    if server_url.is_none() {
        tracing::warn!(
            server_id = %spec.server_id,
            "left out a provider-hosted MCP server with no usable record"
        );
    }
    Ok(server_url)
}

/// Resolve the shared injection policy before adapters construct native wire tools.
/// The counter belongs to the request, so all injected servers share the cap.
pub async fn injected_runtime_tools(
    inventory: &dyn McpInventoryResolver,
    spec: &RemoteMcpToolSpec,
    server_name: &ToolName,
    request_tool_count: &mut usize,
) -> LlmAdapterResult<Vec<(String, RuntimeMcpTool)>> {
    let mut tools =
        inventory
            .list_tools(spec)
            .await
            .map_err(|error| LlmAdapterError::McpInventory {
                server: spec.server_id.clone(),
                message: error.to_string(),
                transient: error.transient,
            })?;
    tools.sort_by(|left, right| left.remote_name.cmp(&right.remote_name));
    let advertised_count = tools.len();
    let tools: Vec<_> = tools
        .into_iter()
        .filter_map(|tool| {
            let name = format!("{server_name}__{}", tool.remote_name);
            crate::tool_catalog::valid_exposed_name(&name).then_some((name, tool))
        })
        .collect();
    let omitted_count = advertised_count - tools.len();
    if omitted_count != 0 {
        tracing::warn!(
            server_id = %spec.server_id,
            omitted_tool_count = omitted_count,
            "omitted runtime MCP tools with provider-incompatible names"
        );
    }
    if request_tool_count.saturating_add(tools.len()) > MAX_RUNTIME_MCP_TOOLS_PER_REQUEST {
        return Err(LlmAdapterError::McpInventory {
            server: spec.server_id.clone(),
            message: "runtime MCP inventory exceeds the per-request tool cap; author a Selected allowlist or switch the record to search exposure".to_owned(),
            transient: false,
        });
    }
    *request_tool_count += tools.len();
    Ok(tools)
}

#[derive(Default)]
pub struct UnconfiguredMcpInventoryResolver;

#[async_trait]
impl McpInventoryResolver for UnconfiguredMcpInventoryResolver {
    async fn list_tools(
        &self,
        spec: &RemoteMcpToolSpec,
    ) -> Result<Vec<RuntimeMcpTool>, McpInventoryError> {
        Err(McpInventoryError::new(format!(
            "runtime MCP inventory resolver is not configured for {}",
            spec.server_id
        )))
    }

    async fn server_url(&self, server_id: &str) -> Result<Option<String>, McpInventoryError> {
        Err(McpInventoryError::new(format!(
            "MCP server resolver is not configured for {server_id}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::{RemoteMcpApprovalPolicy, RemoteMcpExecution, RemoteMcpExposure};
    use serde_json::json;

    struct Inventory(Result<Vec<RuntimeMcpTool>, McpInventoryError>);

    #[async_trait]
    impl McpInventoryResolver for Inventory {
        async fn list_tools(
            &self,
            _: &RemoteMcpToolSpec,
        ) -> Result<Vec<RuntimeMcpTool>, McpInventoryError> {
            self.0.clone()
        }

        async fn server_url(&self, _: &str) -> Result<Option<String>, McpInventoryError> {
            Ok(None)
        }
    }

    fn spec(server: &str) -> RemoteMcpToolSpec {
        RemoteMcpToolSpec {
            server_id: server.to_owned(),
            record_revision: 1,
            server_label: server.to_owned(),
            description_ref: None,
            allowed_tools: None,
            execution: RemoteMcpExecution::Runtime,
            exposure: RemoteMcpExposure::Inject,
            approval: RemoteMcpApprovalPolicy::Never,
            defer_loading: None,
        }
    }

    fn tool(name: &str) -> RuntimeMcpTool {
        RuntimeMcpTool {
            remote_name: name.to_owned(),
            description: Some(format!("Description for {name}")),
            input_schema: json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            output_schema: Some(json!({
                "type": "object",
                "properties": {"matches": {"type": "array", "items": {"type": "string"}}}
            })),
            annotations: Some(json!({"readOnlyHint": true})),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn injected_inventory_sorts_filters_and_preserves_tool_metadata() {
        let inventory = Inventory(Ok(vec![tool("z"), tool("bad.name"), tool("a")]));
        let mut count = 0;
        let tools = injected_runtime_tools(
            &inventory,
            &spec("docs"),
            &ToolName::new("mcp_docs"),
            &mut count,
        )
        .await
        .expect("inventory");
        assert_eq!(
            tools,
            vec![
                ("mcp_docs__a".to_owned(), tool("a")),
                ("mcp_docs__z".to_owned(), tool("z"))
            ]
        );
        assert_eq!(count, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cap_is_cumulative_and_counts_only_provider_compatible_names() {
        let mut count = 0;
        let server_name = ToolName::new("mcp_docs");
        let first = Inventory(Ok((0..MAX_RUNTIME_MCP_TOOLS_PER_REQUEST - 1)
            .map(|i| tool(&format!("read_{i}")))
            .collect()));
        injected_runtime_tools(&first, &spec("first"), &server_name, &mut count)
            .await
            .expect("first server");
        let last = Inventory(Ok(vec![
            tool("last"),
            tool("bad.name"),
            tool(&"x".repeat(64)),
        ]));
        injected_runtime_tools(&last, &spec("second"), &server_name, &mut count)
            .await
            .expect("exact cap");
        assert_eq!(count, MAX_RUNTIME_MCP_TOOLS_PER_REQUEST);
        let error = injected_runtime_tools(
            &Inventory(Ok(vec![tool("extra")])),
            &spec("third"),
            &server_name,
            &mut count,
        )
        .await
        .expect_err("over cap");
        assert!(matches!(error, LlmAdapterError::McpInventory { server, .. } if server == "third"));
        assert_eq!(count, MAX_RUNTIME_MCP_TOOLS_PER_REQUEST);
        let omitted = injected_runtime_tools(
            &Inventory(Ok(vec![tool("bad.name")])),
            &spec("omitted"),
            &server_name,
            &mut count,
        )
        .await
        .expect("filtered tools do not consume quota");
        assert!(omitted.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inventory_errors_keep_server_identity_and_do_not_consume_quota() {
        let mut count = 7;
        let error = injected_runtime_tools(
            &Inventory(Err(McpInventoryError::new("unavailable"))),
            &spec("docs"),
            &ToolName::new("mcp_docs"),
            &mut count,
        )
        .await
        .expect_err("resolver error");
        assert!(
            matches!(error, LlmAdapterError::McpInventory { server, message, .. } if server == "docs" && message == "unavailable")
        );
        assert_eq!(count, 7);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_names_remain_visible_to_catalog_collision_checks() {
        let tools = injected_runtime_tools(
            &Inventory(Ok(vec![tool("read"), tool("read")])),
            &spec("docs"),
            &ToolName::new("mcp_docs"),
            &mut 0,
        )
        .await
        .expect("inventory");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].0, tools[1].0);
    }
}
