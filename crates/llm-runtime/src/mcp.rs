use async_trait::async_trait;
use engine::{RemoteMcpToolSpec, ToolName};
use serde_json::Value;

use crate::{LlmAdapterError, LlmAdapterResult};

pub const MAX_NATIVE_MCP_TOOLS_PER_REQUEST: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub struct NativeMcpTool {
    pub remote_name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    /// Standard MCP annotation hints retained for model-facing discovery.
    /// They are untrusted metadata and never authorize execution or retries.
    pub annotations: Option<Value>,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct McpInventoryError {
    pub message: String,
}

impl McpInventoryError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[async_trait]
pub trait McpInventoryResolver: Send + Sync {
    async fn list_tools(
        &self,
        spec: &RemoteMcpToolSpec,
    ) -> Result<Vec<NativeMcpTool>, McpInventoryError>;
}

/// Resolve the shared injection policy before adapters construct native wire tools.
/// The counter belongs to the request, so all injected servers share the cap.
pub(crate) async fn injected_native_tools(
    inventory: &dyn McpInventoryResolver,
    spec: &RemoteMcpToolSpec,
    server_name: &ToolName,
    request_tool_count: &mut usize,
) -> LlmAdapterResult<Vec<(String, NativeMcpTool)>> {
    let mut native =
        inventory
            .list_tools(spec)
            .await
            .map_err(|error| LlmAdapterError::McpInventory {
                server: spec.server_id.clone(),
                message: error.to_string(),
            })?;
    native.sort_by(|left, right| left.remote_name.cmp(&right.remote_name));
    let advertised_count = native.len();
    let native: Vec<_> = native
        .into_iter()
        .filter_map(|tool| {
            let name = format!("{server_name}__{}", tool.remote_name);
            crate::tool_catalog::valid_exposed_name(&name).then_some((name, tool))
        })
        .collect();
    let omitted_count = advertised_count - native.len();
    if omitted_count != 0 {
        tracing::warn!(
            server_id = %spec.server_id,
            omitted_tool_count = omitted_count,
            "omitted native MCP tools with provider-incompatible names"
        );
    }
    if request_tool_count.saturating_add(native.len()) > MAX_NATIVE_MCP_TOOLS_PER_REQUEST {
        return Err(LlmAdapterError::McpInventory {
            server: spec.server_id.clone(),
            message: "native MCP inventory exceeds the per-request tool cap; author a Selected allowlist or switch the record to search exposure".to_owned(),
        });
    }
    *request_tool_count += native.len();
    Ok(native)
}

#[derive(Default)]
pub struct UnconfiguredMcpInventoryResolver;

#[async_trait]
impl McpInventoryResolver for UnconfiguredMcpInventoryResolver {
    async fn list_tools(
        &self,
        spec: &RemoteMcpToolSpec,
    ) -> Result<Vec<NativeMcpTool>, McpInventoryError> {
        Err(McpInventoryError::new(format!(
            "native MCP inventory resolver is not configured for {}",
            spec.server_id
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{RemoteMcpApprovalPolicy, RemoteMcpExecution, RemoteMcpExposure};
    use serde_json::json;

    struct Inventory(Result<Vec<NativeMcpTool>, McpInventoryError>);

    #[async_trait]
    impl McpInventoryResolver for Inventory {
        async fn list_tools(
            &self,
            _: &RemoteMcpToolSpec,
        ) -> Result<Vec<NativeMcpTool>, McpInventoryError> {
            self.0.clone()
        }
    }

    fn spec(server: &str) -> RemoteMcpToolSpec {
        RemoteMcpToolSpec {
            server_id: server.to_owned(),
            record_revision: 1,
            server_label: server.to_owned(),
            server_url: "https://example.com/mcp".to_owned(),
            description_ref: None,
            allowed_tools: None,
            execution: RemoteMcpExecution::Native,
            exposure: RemoteMcpExposure::Inject,
            approval: RemoteMcpApprovalPolicy::Never,
            defer_loading: None,
            auth_ref: None,
            auth_required: false,
            allow_private_network: false,
        }
    }

    fn tool(name: &str) -> NativeMcpTool {
        NativeMcpTool {
            remote_name: name.to_owned(),
            description: Some(format!("Description for {name}")),
            input_schema: json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            annotations: Some(json!({"readOnlyHint": true})),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn injected_inventory_sorts_filters_and_preserves_tool_metadata() {
        let inventory = Inventory(Ok(vec![tool("z"), tool("bad.name"), tool("a")]));
        let mut count = 0;
        let tools = injected_native_tools(
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
        let first = Inventory(Ok((0..MAX_NATIVE_MCP_TOOLS_PER_REQUEST - 1)
            .map(|i| tool(&format!("read_{i}")))
            .collect()));
        injected_native_tools(&first, &spec("first"), &server_name, &mut count)
            .await
            .expect("first server");
        let last = Inventory(Ok(vec![
            tool("last"),
            tool("bad.name"),
            tool(&"x".repeat(64)),
        ]));
        injected_native_tools(&last, &spec("second"), &server_name, &mut count)
            .await
            .expect("exact cap");
        assert_eq!(count, MAX_NATIVE_MCP_TOOLS_PER_REQUEST);
        let error = injected_native_tools(
            &Inventory(Ok(vec![tool("extra")])),
            &spec("third"),
            &server_name,
            &mut count,
        )
        .await
        .expect_err("over cap");
        assert!(matches!(error, LlmAdapterError::McpInventory { server, .. } if server == "third"));
        assert_eq!(count, MAX_NATIVE_MCP_TOOLS_PER_REQUEST);
        let omitted = injected_native_tools(
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
        let error = injected_native_tools(
            &Inventory(Err(McpInventoryError::new("unavailable"))),
            &spec("docs"),
            &ToolName::new("mcp_docs"),
            &mut count,
        )
        .await
        .expect_err("resolver error");
        assert!(
            matches!(error, LlmAdapterError::McpInventory { server, message } if server == "docs" && message == "unavailable")
        );
        assert_eq!(count, 7);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_names_remain_visible_to_catalog_collision_checks() {
        let tools = injected_native_tools(
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
