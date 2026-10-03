//! Resolve names emitted by scripted models against their admitted request.

use harness::{LlmGenerationRequest, RemoteMcpExecution, RemoteMcpExposure, ToolKind, ToolName};

/// Scripted tests supply their own MCP inventory. Built-in names still come
/// from the real resolver, so test models cannot accidentally rely on registry
/// ids being provider-visible aliases.
pub fn scripted_tool_id(request: &LlmGenerationRequest, name: &str) -> Option<ToolName> {
    let target = tools::runtime::ToolTarget::from(&request.request.model);
    request.request.tools.iter().find_map(|tool| {
        let exposed = match &tool.kind {
            ToolKind::Builtin(spec) => tools::definitions::resolve(&tool.name, spec, &target)
                .expect("scripted tool definition")
                .iter()
                .any(|resolved| resolved.name.as_str() == name && resolved.binding.is_some()),
            ToolKind::Function(_) => tool.name.as_str() == name,
            ToolKind::ProviderNative(spec) => {
                spec.execution == harness::ProviderNativeToolExecution::ClientEffect
                    && tool.name.as_str() == name
            }
            ToolKind::RemoteMcp(spec) => {
                spec.execution == RemoteMcpExecution::Native
                    && spec.exposure == RemoteMcpExposure::Inject
                    && name
                        .strip_prefix(&format!("{}__", tool.name))
                        .is_some_and(|remote_name| {
                            !remote_name.is_empty()
                                && spec.allowed_tools.as_ref().is_none_or(|allowed| {
                                    allowed
                                        .iter()
                                        .any(|allowed_name| allowed_name == remote_name)
                                })
                        })
            }
        };
        exposed.then(|| tool.name.clone())
    })
}
