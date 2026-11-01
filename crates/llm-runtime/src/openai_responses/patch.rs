//! Responses wire presentation for the built-in patch operation. Execution and
//! script calls keep the shared JSON argument contract.

use llm_clients::openai::responses as oai;
use tools::{
    builtin::{BuiltinTool, BuiltinToolOperation, BuiltinToolSurface},
    callable::{CallableBinding, ResolvedTool, ResolvedToolKind},
    runtime::FunctionDefinition,
};

pub(super) fn is_freeform(tool: &ResolvedTool) -> bool {
    let Some(CallableBinding::Builtin { binding }) = &tool.callable_binding else {
        return false;
    };
    matches!(tool.kind, ResolvedToolKind::Function(_))
        && BuiltinTool::from_binding(
            &binding.logical_id,
            binding.adapter_id.as_deref(),
            binding.tool_name.as_str(),
        )
        .is_some_and(|builtin| {
            builtin.operation() == BuiltinToolOperation::ApplyPatch
                && builtin.surface() == BuiltinToolSurface::CodexLike
        })
}

pub(super) fn materialize(function: &FunctionDefinition) -> oai::Tool {
    // The shared definition describes JSON calls made by script tools and by
    // other APIs. Only direct Responses calls use the freeform wire protocol.
    let description = function.description.as_ref().map(|description| {
        description
            .replace(
                "Pass the patch text in the JSON `patch` argument.",
                "For direct calls, pass raw patch text. This is a FREEFORM tool; do not wrap the patch in JSON.",
            )
            .replace(
                "using the input schema above.",
                "passing an object with a `patch` string.",
            )
    });
    oai::Tool::Custom(oai::CustomTool {
        r#type: oai::CustomToolType::Custom,
        name: function.name.to_string(),
        description,
        format: oai::CustomToolFormat::Grammar {
            syntax: "lark".to_owned(),
            definition: include_str!("apply_patch.lark").to_owned(),
        },
    })
}
