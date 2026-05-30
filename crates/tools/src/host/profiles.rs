//! Host tool profile builders.

use std::collections::{BTreeMap, BTreeSet};

use engine::{ModelSelection, ProviderApiKind, ToolName, ToolProfile, ToolProfileId, ToolRegistry};

use crate::{
    error::ToolResult,
    host::{
        context::HostToolContext,
        fs::FileAccessPolicy,
        tools::{HostTool, HostToolOperation, HostToolSurface},
    },
    runtime::{ResolvedToolProfile, ToolCatalog, ToolDocument, ToolExecutionMode, ToolTarget},
};

pub const DIRECT_FS_PROFILE_ID: &str = "host_direct_fs";
pub const CODEX_LIKE_PROFILE_ID: &str = "host_codex";
pub const CLAUDE_CODE_LIKE_PROFILE_ID: &str = "host_claude";
pub const CUSTOM_PROFILE_ID: &str = "host_custom";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostToolPreset {
    DirectFs,
    CodexLike,
    ClaudeCodeLike,
    Custom(HostToolSelection),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostToolSelection {
    pub profile_id: ToolProfileId,
    pub tools: Vec<HostTool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostProfileOptions {
    pub execution: ToolExecutionMode,
}

impl Default for HostProfileOptions {
    fn default() -> Self {
        Self {
            execution: ToolExecutionMode::Inline,
        }
    }
}

impl HostToolSelection {
    pub fn new(profile_id: ToolProfileId, tools: Vec<HostTool>) -> Self {
        Self { profile_id, tools }
    }
}

pub fn recommended_for(ctx: &HostToolContext, target: &ToolTarget) -> HostToolPreset {
    if target.api_kind == ProviderApiKind::AnthropicMessages {
        HostToolPreset::ClaudeCodeLike
    } else if ctx.process.is_some() {
        HostToolPreset::CodexLike
    } else {
        HostToolPreset::DirectFs
    }
}

pub fn recommended_for_model(ctx: &HostToolContext, model: &ModelSelection) -> HostToolPreset {
    recommended_for(ctx, &ToolTarget::from(model))
}

pub fn selection_for_preset(ctx: &HostToolContext, preset: HostToolPreset) -> HostToolSelection {
    match preset {
        HostToolPreset::DirectFs => direct_fs_selection(ctx),
        HostToolPreset::CodexLike => codex_like_selection(ctx),
        HostToolPreset::ClaudeCodeLike => claude_code_like_selection(ctx),
        HostToolPreset::Custom(selection) => selection,
    }
}

fn direct_fs_selection(ctx: &HostToolContext) -> HostToolSelection {
    let mut tools = vec![
        canonical_tool(HostToolOperation::ReadFile),
        canonical_tool(HostToolOperation::Grep),
        canonical_tool(HostToolOperation::Glob),
        canonical_tool(HostToolOperation::ListDir),
    ];

    if !ctx.fs.access_policy().is_read_only() {
        tools.extend([
            canonical_tool(HostToolOperation::WriteFile),
            canonical_tool(HostToolOperation::EditFile),
            canonical_tool(HostToolOperation::ApplyPatch),
        ]);
    }

    HostToolSelection::new(ToolProfileId::new(DIRECT_FS_PROFILE_ID), tools)
}

fn codex_like_selection(ctx: &HostToolContext) -> HostToolSelection {
    let mut tools = vec![codex_like_tool(HostToolOperation::ListDir)];

    if !ctx.fs.access_policy().is_read_only() {
        tools.push(codex_like_tool(HostToolOperation::ApplyPatch));
    }

    if ctx.process.is_some() {
        tools.extend([
            codex_like_tool(HostToolOperation::RunProcess),
            codex_like_tool(HostToolOperation::WriteProcessStdin),
        ]);
    }

    HostToolSelection::new(ToolProfileId::new(CODEX_LIKE_PROFILE_ID), tools)
}

fn claude_code_like_selection(ctx: &HostToolContext) -> HostToolSelection {
    let mut tools = vec![
        claude_code_like_tool(HostToolOperation::ReadFile),
        claude_code_like_tool(HostToolOperation::Grep),
        claude_code_like_tool(HostToolOperation::Glob),
    ];

    if !ctx.fs.access_policy().is_read_only() {
        tools.extend([
            claude_code_like_tool(HostToolOperation::WriteFile),
            claude_code_like_tool(HostToolOperation::EditFile),
        ]);
    }

    if ctx.process.is_some() {
        tools.push(claude_code_like_tool(HostToolOperation::RunProcess));
    }

    HostToolSelection::new(ToolProfileId::new(CLAUDE_CODE_LIKE_PROFILE_ID), tools)
}

fn canonical_tool(operation: HostToolOperation) -> HostTool {
    HostTool::canonical(operation)
}

fn codex_like_tool(operation: HostToolOperation) -> HostTool {
    HostTool::new(operation, HostToolSurface::CodexLike)
}

fn claude_code_like_tool(operation: HostToolOperation) -> HostTool {
    HostTool::new(operation, HostToolSurface::ClaudeCodeLike)
}

pub fn resolve_host_profile(
    ctx: &HostToolContext,
    target: &ToolTarget,
    preset: HostToolPreset,
) -> ToolResult<ResolvedToolProfile> {
    resolve_host_profile_with_options(ctx, target, preset, HostProfileOptions::default())
}

pub fn resolve_host_profile_for_model(
    ctx: &HostToolContext,
    model: &ModelSelection,
    preset: HostToolPreset,
) -> ToolResult<ResolvedToolProfile> {
    resolve_host_profile(ctx, &ToolTarget::from(model), preset)
}

pub fn resolve_host_profile_with_options(
    ctx: &HostToolContext,
    target: &ToolTarget,
    preset: HostToolPreset,
    options: HostProfileOptions,
) -> ToolResult<ResolvedToolProfile> {
    let selection = selection_for_preset(ctx, preset);
    resolve_host_selection_with_options(ctx, target, selection, options)
}

pub fn resolve_host_selection(
    ctx: &HostToolContext,
    target: &ToolTarget,
    selection: HostToolSelection,
) -> ToolResult<ResolvedToolProfile> {
    resolve_host_selection_with_options(ctx, target, selection, HostProfileOptions::default())
}

pub fn resolve_host_selection_with_options(
    ctx: &HostToolContext,
    target: &ToolTarget,
    selection: HostToolSelection,
    options: HostProfileOptions,
) -> ToolResult<ResolvedToolProfile> {
    let scoped_paths = is_scoped_policy(&ctx.fs.access_policy());
    let tools = gate_tools(ctx, selection.tools);
    let mut registry = ToolRegistry::default();
    let mut catalog = ToolCatalog::new();
    let mut documents_by_ref = BTreeMap::new();
    let mut visible_tools = Vec::new();
    let mut seen = BTreeSet::new();

    for tool in tools {
        let tool_name = tool.name(target);
        if !seen.insert(tool_name.clone()) {
            continue;
        }

        let bundle = tool.spec_bundle(target, scoped_paths)?;
        for document in bundle.documents {
            documents_by_ref
                .entry(document.blob_ref.clone())
                .or_insert(document);
        }
        registry.tools.insert(tool_name.clone(), bundle.spec);
        catalog.insert(tool.binding(target, options.execution.clone()));
        visible_tools.push(tool_name);
    }

    registry.profiles.insert(
        selection.profile_id.clone(),
        ToolProfile {
            profile_id: selection.profile_id.clone(),
            visible_tools,
            tool_choice: None,
        },
    );

    Ok(ResolvedToolProfile {
        profile_id: selection.profile_id,
        registry,
        documents: documents_by_ref
            .into_values()
            .collect::<Vec<ToolDocument>>(),
        catalog,
    })
}

fn gate_tools(ctx: &HostToolContext, tools: Vec<HostTool>) -> Vec<HostTool> {
    let read_only = ctx.fs.access_policy().is_read_only();
    tools
        .into_iter()
        .filter(|tool| !tool.requires_write() || !read_only)
        .filter(|tool| !tool.requires_process() || ctx.process.is_some())
        .collect()
}

fn is_scoped_policy(policy: &FileAccessPolicy) -> bool {
    matches!(
        policy,
        FileAccessPolicy::ScopedReadWrite { .. } | FileAccessPolicy::ScopedReadOnly { .. }
    )
}

pub fn tool_names(tools: &[ToolName]) -> Vec<String> {
    tools.iter().map(|name| name.as_str().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use engine::{ProviderApiKind, storage::InMemoryBlobStore};

    use super::*;
    use crate::host::{
        fs::{FileAccessPolicy, InMemoryFileSystem},
        process::{
            ProcessError, ProcessExecResult, ProcessExecutor, ProcessOutput, ProcessRequest,
            WriteProcessStdinRequest,
        },
    };

    struct StubProcessExecutor;

    #[async_trait]
    impl ProcessExecutor for StubProcessExecutor {
        async fn run_process(&self, _request: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
            Err(ProcessError::Unsupported {
                message: "not implemented".to_string(),
            })
        }

        async fn write_stdin(
            &self,
            _request: WriteProcessStdinRequest,
        ) -> ProcessExecResult<ProcessOutput> {
            Err(ProcessError::Unsupported {
                message: "not implemented".to_string(),
            })
        }
    }

    fn target() -> ToolTarget {
        ToolTarget::api_kind(ProviderApiKind::OpenAiResponses)
    }

    fn context(policy: FileAccessPolicy, process: bool) -> HostToolContext {
        let process = process.then(|| Arc::new(StubProcessExecutor) as Arc<dyn ProcessExecutor>);
        HostToolContext::new(
            Arc::new(InMemoryFileSystem::new(policy)),
            process,
            Arc::new(InMemoryBlobStore::new()),
        )
    }

    fn profile_names(registry: &ToolRegistry, profile_id: &str) -> Vec<String> {
        let profile = registry
            .profiles
            .get(&ToolProfileId::new(profile_id))
            .expect("profile");
        tool_names(&profile.visible_tools)
    }

    #[test]
    fn direct_fs_selection_includes_write_tools_for_writable_filesystem() {
        let selection = direct_fs_selection(&context(FileAccessPolicy::FullReadWrite, false));

        assert!(
            selection
                .tools
                .contains(&canonical_tool(HostToolOperation::WriteFile))
        );
        assert!(
            selection
                .tools
                .contains(&canonical_tool(HostToolOperation::EditFile))
        );
        assert!(
            selection
                .tools
                .contains(&canonical_tool(HostToolOperation::ApplyPatch))
        );
    }

    #[test]
    fn direct_fs_selection_omits_write_tools_for_read_only_filesystem() {
        let selection = direct_fs_selection(&context(FileAccessPolicy::FullReadOnly, false));

        assert!(
            selection
                .tools
                .contains(&canonical_tool(HostToolOperation::ReadFile))
        );
        assert!(
            !selection
                .tools
                .contains(&canonical_tool(HostToolOperation::WriteFile))
        );
        assert!(
            !selection
                .tools
                .contains(&canonical_tool(HostToolOperation::EditFile))
        );
        assert!(
            !selection
                .tools
                .contains(&canonical_tool(HostToolOperation::ApplyPatch))
        );
    }

    #[test]
    fn codex_like_selection_omits_process_tools_without_process_capability() {
        let selection = codex_like_selection(&context(FileAccessPolicy::FullReadWrite, false));

        assert!(
            selection
                .tools
                .contains(&codex_like_tool(HostToolOperation::ApplyPatch))
        );
        assert!(
            !selection
                .tools
                .contains(&codex_like_tool(HostToolOperation::RunProcess))
        );
        assert!(
            !selection
                .tools
                .contains(&codex_like_tool(HostToolOperation::WriteProcessStdin))
        );
    }

    #[test]
    fn codex_like_selection_includes_process_tools_with_process_capability() {
        let selection = codex_like_selection(&context(FileAccessPolicy::FullReadWrite, true));

        assert!(
            selection
                .tools
                .contains(&codex_like_tool(HostToolOperation::RunProcess))
        );
        assert!(
            selection
                .tools
                .contains(&codex_like_tool(HostToolOperation::WriteProcessStdin))
        );
    }

    #[test]
    fn codex_like_selection_omits_apply_patch_for_read_only_filesystem() {
        let selection = codex_like_selection(&context(FileAccessPolicy::FullReadOnly, true));

        assert!(
            selection
                .tools
                .contains(&codex_like_tool(HostToolOperation::ListDir))
        );
        assert!(
            !selection
                .tools
                .contains(&codex_like_tool(HostToolOperation::ApplyPatch))
        );
    }

    #[test]
    fn claude_code_like_selection_uses_claude_surface_and_gates_capabilities() {
        let selection = claude_code_like_selection(&context(FileAccessPolicy::FullReadOnly, true));

        assert!(
            selection
                .tools
                .contains(&claude_code_like_tool(HostToolOperation::ReadFile))
        );
        assert!(
            selection
                .tools
                .contains(&claude_code_like_tool(HostToolOperation::Grep))
        );
        assert!(
            selection
                .tools
                .contains(&claude_code_like_tool(HostToolOperation::Glob))
        );
        assert!(
            selection
                .tools
                .contains(&claude_code_like_tool(HostToolOperation::RunProcess))
        );
        assert!(
            !selection
                .tools
                .contains(&claude_code_like_tool(HostToolOperation::WriteFile))
        );
        assert!(
            !selection
                .tools
                .contains(&claude_code_like_tool(HostToolOperation::EditFile))
        );
    }

    #[test]
    fn direct_fs_profile_gates_write_tools_for_read_only_filesystem() {
        let ctx = context(FileAccessPolicy::FullReadOnly, false);
        let profile =
            resolve_host_profile(&ctx, &target(), HostToolPreset::DirectFs).expect("profile");

        profile.registry.validate().expect("valid registry");
        let names = profile_names(&profile.registry, DIRECT_FS_PROFILE_ID);
        assert_eq!(names, vec!["read_file", "grep", "glob", "list_dir"]);
        assert!(
            !profile
                .registry
                .tools
                .contains_key(&ToolName::new("write_file"))
        );
        assert!(profile.catalog.get(&ToolName::new("write_file")).is_none());
    }

    #[test]
    fn direct_fs_profile_includes_catalog_binding_for_visible_tool() {
        let ctx = context(FileAccessPolicy::FullReadWrite, false);
        let profile =
            resolve_host_profile(&ctx, &target(), HostToolPreset::DirectFs).expect("profile");

        profile.registry.validate().expect("valid registry");
        let binding = profile
            .catalog
            .get(&ToolName::new("read_file"))
            .expect("binding");
        assert_eq!(binding.logical_id, "host.read_file");
        assert_eq!(binding.activity_type, "forge.host.read_file");
        assert_eq!(binding.execution, ToolExecutionMode::Inline);
    }

    #[test]
    fn profile_options_can_mark_bindings_as_activity_backed() {
        let ctx = context(FileAccessPolicy::FullReadWrite, false);
        let profile = resolve_host_profile_with_options(
            &ctx,
            &target(),
            HostToolPreset::DirectFs,
            HostProfileOptions {
                execution: ToolExecutionMode::Activity,
            },
        )
        .expect("profile");

        let binding = profile
            .catalog
            .get(&ToolName::new("read_file"))
            .expect("binding");
        assert_eq!(binding.execution, ToolExecutionMode::Activity);
    }

    #[test]
    fn codex_like_profile_gates_process_tools() {
        let ctx = context(FileAccessPolicy::FullReadWrite, false);
        let profile =
            resolve_host_profile(&ctx, &target(), HostToolPreset::CodexLike).expect("profile");

        profile.registry.validate().expect("valid registry");
        let names = profile_names(&profile.registry, CODEX_LIKE_PROFILE_ID);
        assert_eq!(names, vec!["list_dir", "apply_patch"]);
        let binding = profile
            .catalog
            .get(&ToolName::new("apply_patch"))
            .expect("binding");
        assert_eq!(binding.logical_id, "host.codex.apply_patch");
    }

    #[test]
    fn claude_code_like_profile_uses_claude_tool_names_and_bindings() {
        let ctx = context(FileAccessPolicy::FullReadWrite, true);
        let profile =
            resolve_host_profile(&ctx, &target(), HostToolPreset::ClaudeCodeLike).expect("profile");

        profile.registry.validate().expect("valid registry");
        let names = profile_names(&profile.registry, CLAUDE_CODE_LIKE_PROFILE_ID);
        assert_eq!(names, vec!["Read", "Grep", "Glob", "Write", "Edit", "Bash"]);
        let binding = profile
            .catalog
            .get(&ToolName::new("Read"))
            .expect("binding");
        assert_eq!(binding.logical_id, "host.claude.read_file");
    }

    #[test]
    fn recommended_profile_uses_claude_surface_for_anthropic_messages() {
        let ctx = context(FileAccessPolicy::FullReadWrite, false);
        let target = ToolTarget::api_kind(ProviderApiKind::AnthropicMessages);

        assert_eq!(
            recommended_for(&ctx, &target),
            HostToolPreset::ClaudeCodeLike
        );
    }

    #[test]
    fn custom_profile_applies_capability_gating() {
        let ctx = context(FileAccessPolicy::FullReadOnly, false);
        let profile = resolve_host_profile(
            &ctx,
            &target(),
            HostToolPreset::Custom(HostToolSelection::new(
                ToolProfileId::new("custom_test"),
                vec![
                    canonical_tool(HostToolOperation::ReadFile),
                    canonical_tool(HostToolOperation::WriteFile),
                    canonical_tool(HostToolOperation::RunProcess),
                ],
            )),
        )
        .expect("profile");

        profile.registry.validate().expect("valid registry");
        let names = profile_names(&profile.registry, "custom_test");
        assert_eq!(names, vec!["read_file"]);
    }
}
