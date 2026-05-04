//! Host tool definitions and surface routing.

use agent_core::{
    FunctionToolSpec, ToolKind, ToolName, ToolParallelism, ToolSpec, ToolTargetRequirement,
};
use serde_json::Value;

use crate::{
    error::{ToolError, ToolResult},
    host::context::HostToolContext,
    runtime::{
        ToolBinding, ToolDocument, ToolExecutionMode, ToolInvocationOutput, ToolSpecBundle,
        ToolTarget,
    },
};

mod canonical;
mod claude;
mod codex;
mod shared;

pub mod apply_patch;
pub mod edit_file;
pub mod glob;
pub mod grep;
pub mod list_dir;
pub mod read_file;
pub mod run_process;
pub mod write_file;
pub mod write_process_stdin;

pub use apply_patch::{ApplyPatchArgs, ApplyPatchResult, invoke_apply_patch};
pub use edit_file::{EditFileArgs, EditFileResult, invoke_edit_file};
pub use glob::{GlobArgs, GlobResult, invoke_glob};
pub use grep::{GrepArgs, GrepMatch, GrepResult, invoke_grep};
pub use list_dir::{ListDirArgs, ListDirEntry, ListDirResult, invoke_list_dir};
pub use read_file::{ReadFileArgs, ReadFileResult, invoke_read_file};
pub use run_process::{RunProcessArgs, invoke_run_process};
pub use write_file::{WriteFileArgs, WriteFileResult, invoke_write_file};
pub use write_process_stdin::{WriteProcessStdinArgs, invoke_write_process_stdin};

pub(crate) use shared::{
    collect_file_paths, invalid_request, relative_path_string, resolve_path, unsupported_capability,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum HostToolOperation {
    ReadFile,
    WriteFile,
    EditFile,
    ApplyPatch,
    Grep,
    Glob,
    ListDir,
    RunProcess,
    WriteProcessStdin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum HostToolSurface {
    Canonical,
    CodexLike,
    ClaudeCodeLike,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct HostTool {
    operation: HostToolOperation,
    surface: HostToolSurface,
}

impl HostTool {
    pub const fn new(operation: HostToolOperation, surface: HostToolSurface) -> Self {
        Self { operation, surface }
    }

    pub const fn canonical(operation: HostToolOperation) -> Self {
        Self::new(operation, HostToolSurface::Canonical)
    }

    pub const fn operation(self) -> HostToolOperation {
        self.operation
    }

    pub const fn surface(self) -> HostToolSurface {
        self.surface
    }

    pub const fn logical_id(self) -> &'static str {
        match (self.surface, self.operation) {
            (HostToolSurface::Canonical, HostToolOperation::ReadFile) => "host.read_file",
            (HostToolSurface::Canonical, HostToolOperation::WriteFile) => "host.write_file",
            (HostToolSurface::Canonical, HostToolOperation::EditFile) => "host.edit_file",
            (HostToolSurface::Canonical, HostToolOperation::ApplyPatch) => "host.apply_patch",
            (HostToolSurface::Canonical, HostToolOperation::Grep) => "host.grep",
            (HostToolSurface::Canonical, HostToolOperation::Glob) => "host.glob",
            (HostToolSurface::Canonical, HostToolOperation::ListDir) => "host.list_dir",
            (HostToolSurface::Canonical, HostToolOperation::RunProcess) => "host.run_process",
            (HostToolSurface::Canonical, HostToolOperation::WriteProcessStdin) => {
                "host.write_process_stdin"
            }
            (HostToolSurface::CodexLike, HostToolOperation::ReadFile) => "host.codex.read_file",
            (HostToolSurface::CodexLike, HostToolOperation::WriteFile) => "host.codex.write_file",
            (HostToolSurface::CodexLike, HostToolOperation::EditFile) => "host.codex.edit_file",
            (HostToolSurface::CodexLike, HostToolOperation::ApplyPatch) => "host.codex.apply_patch",
            (HostToolSurface::CodexLike, HostToolOperation::Grep) => "host.codex.grep",
            (HostToolSurface::CodexLike, HostToolOperation::Glob) => "host.codex.glob",
            (HostToolSurface::CodexLike, HostToolOperation::ListDir) => "host.codex.list_dir",
            (HostToolSurface::CodexLike, HostToolOperation::RunProcess) => "host.codex.run_process",
            (HostToolSurface::CodexLike, HostToolOperation::WriteProcessStdin) => {
                "host.codex.write_process_stdin"
            }
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::ReadFile) => {
                "host.claude.read_file"
            }
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::WriteFile) => {
                "host.claude.write_file"
            }
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::EditFile) => {
                "host.claude.edit_file"
            }
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::ApplyPatch) => {
                "host.claude.apply_patch"
            }
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::Grep) => "host.claude.grep",
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::Glob) => "host.claude.glob",
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::ListDir) => "host.claude.list_dir",
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::RunProcess) => {
                "host.claude.run_process"
            }
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::WriteProcessStdin) => {
                "host.claude.write_process_stdin"
            }
        }
    }

    pub const fn activity_type(self) -> &'static str {
        match self.operation {
            HostToolOperation::ReadFile => "forge.host.read_file",
            HostToolOperation::WriteFile => "forge.host.write_file",
            HostToolOperation::EditFile => "forge.host.edit_file",
            HostToolOperation::ApplyPatch => "forge.host.apply_patch",
            HostToolOperation::Grep => "forge.host.grep",
            HostToolOperation::Glob => "forge.host.glob",
            HostToolOperation::ListDir => "forge.host.list_dir",
            HostToolOperation::RunProcess => "forge.host.run_process",
            HostToolOperation::WriteProcessStdin => "forge.host.write_process_stdin",
        }
    }

    pub const fn name_str(self) -> &'static str {
        match (self.surface, self.operation) {
            (
                HostToolSurface::Canonical | HostToolSurface::CodexLike,
                HostToolOperation::ReadFile,
            ) => "read_file",
            (
                HostToolSurface::Canonical | HostToolSurface::CodexLike,
                HostToolOperation::WriteFile,
            ) => "write_file",
            (
                HostToolSurface::Canonical | HostToolSurface::CodexLike,
                HostToolOperation::EditFile,
            ) => "edit_file",
            (
                HostToolSurface::Canonical | HostToolSurface::CodexLike,
                HostToolOperation::ApplyPatch,
            ) => "apply_patch",
            (HostToolSurface::Canonical | HostToolSurface::CodexLike, HostToolOperation::Grep) => {
                "grep"
            }
            (HostToolSurface::Canonical | HostToolSurface::CodexLike, HostToolOperation::Glob) => {
                "glob"
            }
            (
                HostToolSurface::Canonical | HostToolSurface::CodexLike,
                HostToolOperation::ListDir,
            ) => "list_dir",
            (
                HostToolSurface::Canonical | HostToolSurface::CodexLike,
                HostToolOperation::RunProcess,
            ) => "exec_command",
            (
                HostToolSurface::Canonical | HostToolSurface::CodexLike,
                HostToolOperation::WriteProcessStdin,
            ) => "write_stdin",
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::ReadFile) => "Read",
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::WriteFile) => "Write",
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::EditFile) => "Edit",
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::Grep) => "Grep",
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::Glob) => "Glob",
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::RunProcess) => "Bash",
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::ApplyPatch) => "apply_patch",
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::ListDir) => "list_dir",
            (HostToolSurface::ClaudeCodeLike, HostToolOperation::WriteProcessStdin) => {
                "write_stdin"
            }
        }
    }

    pub fn name(self, _target: &ToolTarget) -> ToolName {
        ToolName::new(self.name_str())
    }

    pub fn from_logical_id(logical_id: &str) -> Option<Self> {
        Some(match logical_id {
            "host.read_file" => Self::canonical(HostToolOperation::ReadFile),
            "host.write_file" => Self::canonical(HostToolOperation::WriteFile),
            "host.edit_file" => Self::canonical(HostToolOperation::EditFile),
            "host.apply_patch" => Self::canonical(HostToolOperation::ApplyPatch),
            "host.grep" => Self::canonical(HostToolOperation::Grep),
            "host.glob" => Self::canonical(HostToolOperation::Glob),
            "host.list_dir" => Self::canonical(HostToolOperation::ListDir),
            "host.run_process" => Self::canonical(HostToolOperation::RunProcess),
            "host.write_process_stdin" => Self::canonical(HostToolOperation::WriteProcessStdin),
            "host.codex.read_file" => {
                Self::new(HostToolOperation::ReadFile, HostToolSurface::CodexLike)
            }
            "host.codex.write_file" => {
                Self::new(HostToolOperation::WriteFile, HostToolSurface::CodexLike)
            }
            "host.codex.edit_file" => {
                Self::new(HostToolOperation::EditFile, HostToolSurface::CodexLike)
            }
            "host.codex.apply_patch" => {
                Self::new(HostToolOperation::ApplyPatch, HostToolSurface::CodexLike)
            }
            "host.codex.grep" => Self::new(HostToolOperation::Grep, HostToolSurface::CodexLike),
            "host.codex.glob" => Self::new(HostToolOperation::Glob, HostToolSurface::CodexLike),
            "host.codex.list_dir" => {
                Self::new(HostToolOperation::ListDir, HostToolSurface::CodexLike)
            }
            "host.codex.run_process" => {
                Self::new(HostToolOperation::RunProcess, HostToolSurface::CodexLike)
            }
            "host.codex.write_process_stdin" => Self::new(
                HostToolOperation::WriteProcessStdin,
                HostToolSurface::CodexLike,
            ),
            "host.claude.read_file" => {
                Self::new(HostToolOperation::ReadFile, HostToolSurface::ClaudeCodeLike)
            }
            "host.claude.write_file" => Self::new(
                HostToolOperation::WriteFile,
                HostToolSurface::ClaudeCodeLike,
            ),
            "host.claude.edit_file" => {
                Self::new(HostToolOperation::EditFile, HostToolSurface::ClaudeCodeLike)
            }
            "host.claude.grep" => {
                Self::new(HostToolOperation::Grep, HostToolSurface::ClaudeCodeLike)
            }
            "host.claude.glob" => {
                Self::new(HostToolOperation::Glob, HostToolSurface::ClaudeCodeLike)
            }
            "host.claude.run_process" => Self::new(
                HostToolOperation::RunProcess,
                HostToolSurface::ClaudeCodeLike,
            ),
            _ => return None,
        })
    }

    pub const fn requires_write(self) -> bool {
        matches!(
            self.operation,
            HostToolOperation::WriteFile
                | HostToolOperation::EditFile
                | HostToolOperation::ApplyPatch
        )
    }

    pub const fn requires_process(self) -> bool {
        matches!(
            self.operation,
            HostToolOperation::RunProcess | HostToolOperation::WriteProcessStdin
        )
    }

    pub const fn parallelism(self) -> ToolParallelism {
        match self.operation {
            HostToolOperation::ReadFile
            | HostToolOperation::Grep
            | HostToolOperation::Glob
            | HostToolOperation::ListDir => ToolParallelism::ParallelSafe,
            HostToolOperation::WriteFile
            | HostToolOperation::EditFile
            | HostToolOperation::ApplyPatch
            | HostToolOperation::RunProcess
            | HostToolOperation::WriteProcessStdin => ToolParallelism::Exclusive,
        }
    }

    pub fn binding(self, target: &ToolTarget, execution: ToolExecutionMode) -> ToolBinding {
        ToolBinding::new(
            self.name(target),
            self.logical_id(),
            self.activity_type(),
            execution,
            self.parallelism(),
        )
    }

    pub fn spec_bundle(
        self,
        target: &ToolTarget,
        scoped_paths: bool,
    ) -> ToolResult<ToolSpecBundle> {
        let description =
            ToolDocument::text("text/plain; charset=utf-8", self.description(scoped_paths)?);
        let input_schema = ToolDocument::text(
            "application/schema+json",
            serde_json::to_string(&self.input_schema(target)?).map_err(|error| {
                ToolError::InvalidRequest {
                    message: format!("failed to encode tool schema: {error}"),
                }
            })?,
        );
        Ok(ToolSpecBundle {
            spec: ToolSpec {
                name: self.name(target),
                kind: ToolKind::Function(FunctionToolSpec {
                    model_name: None,
                    description_ref: Some(description.blob_ref.clone()),
                    input_schema_ref: input_schema.blob_ref.clone(),
                    output_schema_ref: None,
                    strict: Some(false),
                    provider_options_ref: None,
                }),
                parallelism: self.parallelism(),
                target_requirement: ToolTargetRequirement::required("host"),
            },
            documents: vec![description, input_schema],
        })
    }

    fn description(self, scoped_paths: bool) -> ToolResult<String> {
        match self.surface {
            HostToolSurface::Canonical => Ok(canonical::description(self.operation, scoped_paths)),
            HostToolSurface::CodexLike => Ok(codex::description(self.operation, scoped_paths)),
            HostToolSurface::ClaudeCodeLike => claude::description(self.operation, scoped_paths),
        }
    }

    fn input_schema(self, _target: &ToolTarget) -> ToolResult<Value> {
        match self.surface {
            HostToolSurface::Canonical => Ok(canonical::input_schema(self.operation)),
            HostToolSurface::CodexLike => Ok(codex::input_schema(self.operation)),
            HostToolSurface::ClaudeCodeLike => claude::input_schema(self.operation),
        }
    }

    pub async fn invoke_json(
        self,
        ctx: &HostToolContext,
        arguments: Value,
    ) -> ToolResult<ToolInvocationOutput> {
        match self.surface {
            HostToolSurface::Canonical => {
                canonical::invoke_json(self.operation, ctx, arguments).await
            }
            HostToolSurface::CodexLike => codex::invoke_json(self.operation, ctx, arguments).await,
            HostToolSurface::ClaudeCodeLike => {
                claude::invoke_json(self.operation, ctx, arguments).await
            }
        }
    }
}

impl HostToolOperation {
    pub(super) fn name_for_error(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::WriteFile => "write_file",
            Self::EditFile => "edit_file",
            Self::ApplyPatch => "apply_patch",
            Self::Grep => "grep",
            Self::Glob => "glob",
            Self::ListDir => "list_dir",
            Self::RunProcess => "run_process",
            Self::WriteProcessStdin => "write_process_stdin",
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_core::{ProviderApiKind, ToolKind};
    use serde_json::json;

    use super::*;
    use crate::{host::fs::FsPath, runtime::decode_args};

    fn target() -> ToolTarget {
        ToolTarget::api_kind(ProviderApiKind::OpenAiResponses)
    }

    #[test]
    fn host_tool_names_are_valid_tool_names() {
        for tool in [
            HostTool::canonical(HostToolOperation::ReadFile),
            HostTool::canonical(HostToolOperation::WriteFile),
            HostTool::canonical(HostToolOperation::EditFile),
            HostTool::canonical(HostToolOperation::ApplyPatch),
            HostTool::canonical(HostToolOperation::Grep),
            HostTool::canonical(HostToolOperation::Glob),
            HostTool::canonical(HostToolOperation::ListDir),
            HostTool::canonical(HostToolOperation::RunProcess),
            HostTool::canonical(HostToolOperation::WriteProcessStdin),
        ] {
            assert_eq!(tool.name(&target()).as_str(), tool.name_str());
        }
    }

    #[test]
    fn spec_bundle_uses_content_addressed_documents() {
        let bundle = HostTool::canonical(HostToolOperation::ReadFile)
            .spec_bundle(&target(), true)
            .expect("spec bundle");

        let ToolKind::Function(function) = bundle.spec.kind else {
            panic!("expected function tool");
        };
        assert_eq!(bundle.documents.len(), 2);
        assert_eq!(
            function.description_ref,
            Some(bundle.documents[0].blob_ref.clone())
        );
        assert_eq!(function.input_schema_ref, bundle.documents[1].blob_ref);
        assert!(
            bundle.documents[0]
                .text_lossy()
                .contains("configured filesystem scope")
        );
        assert!(bundle.documents[1].text_lossy().contains("\"path\""));
    }

    #[test]
    fn claude_code_like_surface_generates_claude_style_schema() {
        let tool = HostTool::new(HostToolOperation::ReadFile, HostToolSurface::ClaudeCodeLike);

        assert_eq!(tool.name_str(), "Read");
        let bundle = tool.spec_bundle(&target(), false).expect("spec bundle");
        assert!(bundle.documents[1].text_lossy().contains("\"file_path\""));
    }

    #[test]
    fn claude_code_like_surface_rejects_unmapped_operations() {
        let tool = HostTool::new(
            HostToolOperation::ApplyPatch,
            HostToolSurface::ClaudeCodeLike,
        );

        assert!(matches!(
            tool.spec_bundle(&target(), false),
            Err(ToolError::UnsupportedCapability { .. })
        ));
    }

    #[test]
    fn canonical_args_default_model_omitted_convenience_fields() {
        let list: ListDirArgs = decode_args(json!({})).expect("list args");
        assert_eq!(list.path, FsPath::root());

        let grep: GrepArgs = decode_args(json!({ "pattern": "struct Foo" })).expect("grep args");
        assert!(!grep.case_sensitive);

        let edit: EditFileArgs = decode_args(json!({
            "path": "src/lib.rs",
            "old_string": "before",
            "new_string": "after"
        }))
        .expect("edit args");
        assert!(!edit.replace_all);

        let run: RunProcessArgs =
            decode_args(json!({ "argv": ["cargo", "test"] })).expect("run args");
        assert!(run.env.is_empty());

        let stdin: WriteProcessStdinArgs =
            decode_args(json!({ "handle": "proc-1", "input": "q" })).expect("stdin args");
        assert_eq!(stdin.handle.as_str(), "proc-1");
        assert!(!stdin.close_stdin);
    }
}
