//! Resolve names emitted by scripted models against their admitted request.

use harness::{LlmGenerationRequest, ToolId, ToolKind};

/// Scripted tests supply their own MCP inventory. Built-in names still come
/// from the real resolver, so test models cannot accidentally rely on registry
/// ids being provider-visible aliases.
pub fn scripted_tool_id(request: &LlmGenerationRequest, name: &str) -> Option<ToolId> {
    let target = tools::runtime::ToolTarget::from(&request.request.model);
    request.request.tools.iter().find_map(|tool| {
        let exposed = match &tool.kind {
            ToolKind::Builtin(spec) => tools::definitions::resolve(&tool.id, spec, &target)
                .expect("scripted tool definition")
                .iter()
                .any(|resolved| resolved.name.as_str() == name && resolved.binding.is_some()),
            ToolKind::Function(_) => tool.id.as_str() == name,
            ToolKind::ProviderNative(spec) => {
                spec.execution == harness::ProviderNativeToolExecution::ClientEffect
                    && tool.id.as_str() == name
            }
            ToolKind::RemoteMcp(_) => false,
        };
        exposed.then(|| tool.id.clone())
    })
}
