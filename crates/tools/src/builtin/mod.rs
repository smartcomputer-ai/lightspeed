//! Built-in filesystem and environment action tool definitions.

use engine::{
    FunctionToolSpec, ToolKind, ToolName, ToolParallelism, ToolSpec, ToolTargetRequirement,
};
use serde_json::Value;

use crate::{
    error::{ToolError, ToolResult},
    runtime::{
        ToolBinding, ToolDocument, ToolExecutionMode, ToolInvocationOutput, ToolSpecBundle,
        ToolTarget,
    },
    targets::{ENV_TARGET_NAMESPACE, FS_TARGET_NAMESPACE, ResolvedToolContext},
};

mod canonical;
mod claude;
mod codex;
mod shared;

pub use crate::environment::tools::{
    RunProcessArgs, WriteProcessStdinArgs, invoke_job_cancel, invoke_job_list, invoke_job_read,
    invoke_job_start, invoke_job_wait, invoke_run_process, invoke_write_process_stdin,
};
pub use crate::fs::tools::{
    ApplyPatchArgs, ApplyPatchResult, EditFileArgs, EditFileResult, GlobArgs, GlobResult, GrepArgs,
    GrepMatch, GrepResult, ListDirArgs, ListDirEntry, ReadFileArgs, ReadFileResult, WriteFileArgs,
    WriteFileResult, invoke_apply_patch, invoke_edit_file, invoke_glob, invoke_grep,
    invoke_list_dir, invoke_read_file, invoke_write_file,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum BuiltinToolOperation {
    ReadFile,
    WriteFile,
    EditFile,
    ApplyPatch,
    Grep,
    Glob,
    ListDir,
    RunProcess,
    WriteProcessStdin,
    JobStart,
    JobList,
    JobRead,
    JobWait,
    JobCancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum BuiltinToolSurface {
    Canonical,
    CodexLike,
    ClaudeCodeLike,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct BuiltinTool {
    operation: BuiltinToolOperation,
    surface: BuiltinToolSurface,
}

impl BuiltinTool {
    pub const fn new(operation: BuiltinToolOperation, surface: BuiltinToolSurface) -> Self {
        Self { operation, surface }
    }

    pub const fn canonical(operation: BuiltinToolOperation) -> Self {
        Self::new(operation, BuiltinToolSurface::Canonical)
    }

    pub const fn operation(self) -> BuiltinToolOperation {
        self.operation
    }

    pub const fn surface(self) -> BuiltinToolSurface {
        self.surface
    }

    pub const fn logical_id(self) -> &'static str {
        match (self.surface, self.operation) {
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::ReadFile) => "fs.read_file",
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::WriteFile) => "fs.write_file",
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::EditFile) => "fs.edit_file",
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::ApplyPatch) => "fs.apply_patch",
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::Grep) => "fs.grep",
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::Glob) => "fs.glob",
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::ListDir) => "fs.list_dir",
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::RunProcess) => "env.run_process",
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::WriteProcessStdin) => {
                "env.write_process_stdin"
            }
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::JobStart) => "env.job_start",
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::JobList) => "env.job_list",
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::JobRead) => "env.job_read",
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::JobWait) => "env.job_wait",
            (BuiltinToolSurface::Canonical, BuiltinToolOperation::JobCancel) => "env.job_cancel",
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::ReadFile) => "fs.codex.read_file",
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::WriteFile) => {
                "fs.codex.write_file"
            }
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::EditFile) => "fs.codex.edit_file",
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::ApplyPatch) => {
                "fs.codex.apply_patch"
            }
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::Grep) => "fs.codex.grep",
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::Glob) => "fs.codex.glob",
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::ListDir) => "fs.codex.list_dir",
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::RunProcess) => {
                "env.codex.run_process"
            }
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::WriteProcessStdin) => {
                "env.codex.write_process_stdin"
            }
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::JobStart) => {
                "env.codex.job_start"
            }
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::JobList) => "env.codex.job_list",
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::JobRead) => "env.codex.job_read",
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::JobWait) => "env.codex.job_wait",
            (BuiltinToolSurface::CodexLike, BuiltinToolOperation::JobCancel) => {
                "env.codex.job_cancel"
            }
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::ReadFile) => {
                "fs.claude.read_file"
            }
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::WriteFile) => {
                "fs.claude.write_file"
            }
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::EditFile) => {
                "fs.claude.edit_file"
            }
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::ApplyPatch) => {
                "fs.claude.apply_patch"
            }
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::Grep) => "fs.claude.grep",
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::Glob) => "fs.claude.glob",
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::ListDir) => {
                "fs.claude.list_dir"
            }
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::RunProcess) => {
                "env.claude.run_process"
            }
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::WriteProcessStdin) => {
                "env.claude.write_process_stdin"
            }
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::JobStart) => {
                "env.claude.job_start"
            }
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::JobList) => {
                "env.claude.job_list"
            }
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::JobRead) => {
                "env.claude.job_read"
            }
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::JobWait) => {
                "env.claude.job_wait"
            }
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::JobCancel) => {
                "env.claude.job_cancel"
            }
        }
    }

    pub const fn activity_type(self) -> &'static str {
        match self.operation {
            BuiltinToolOperation::ReadFile => "lightspeed.fs.read_file",
            BuiltinToolOperation::WriteFile => "lightspeed.fs.write_file",
            BuiltinToolOperation::EditFile => "lightspeed.fs.edit_file",
            BuiltinToolOperation::ApplyPatch => "lightspeed.fs.apply_patch",
            BuiltinToolOperation::Grep => "lightspeed.fs.grep",
            BuiltinToolOperation::Glob => "lightspeed.fs.glob",
            BuiltinToolOperation::ListDir => "lightspeed.fs.list_dir",
            BuiltinToolOperation::RunProcess => "lightspeed.env.run_process",
            BuiltinToolOperation::WriteProcessStdin => "lightspeed.env.write_process_stdin",
            BuiltinToolOperation::JobStart => "lightspeed.env.job_start",
            BuiltinToolOperation::JobList => "lightspeed.env.job_list",
            BuiltinToolOperation::JobRead => "lightspeed.env.job_read",
            BuiltinToolOperation::JobWait => "lightspeed.env.job_wait",
            BuiltinToolOperation::JobCancel => "lightspeed.env.job_cancel",
        }
    }

    pub const fn name_str(self) -> &'static str {
        match (self.surface, self.operation) {
            (
                BuiltinToolSurface::Canonical | BuiltinToolSurface::CodexLike,
                BuiltinToolOperation::ReadFile,
            ) => "read_file",
            (
                BuiltinToolSurface::Canonical | BuiltinToolSurface::CodexLike,
                BuiltinToolOperation::WriteFile,
            ) => "write_file",
            (
                BuiltinToolSurface::Canonical | BuiltinToolSurface::CodexLike,
                BuiltinToolOperation::EditFile,
            ) => "edit_file",
            (
                BuiltinToolSurface::Canonical | BuiltinToolSurface::CodexLike,
                BuiltinToolOperation::ApplyPatch,
            ) => "apply_patch",
            (
                BuiltinToolSurface::Canonical | BuiltinToolSurface::CodexLike,
                BuiltinToolOperation::Grep,
            ) => "grep",
            (
                BuiltinToolSurface::Canonical | BuiltinToolSurface::CodexLike,
                BuiltinToolOperation::Glob,
            ) => "glob",
            (
                BuiltinToolSurface::Canonical | BuiltinToolSurface::CodexLike,
                BuiltinToolOperation::ListDir,
            ) => "list_dir",
            (
                BuiltinToolSurface::Canonical | BuiltinToolSurface::CodexLike,
                BuiltinToolOperation::RunProcess,
            ) => "exec_command",
            (
                BuiltinToolSurface::Canonical | BuiltinToolSurface::CodexLike,
                BuiltinToolOperation::WriteProcessStdin,
            ) => "write_stdin",
            (
                BuiltinToolSurface::Canonical
                | BuiltinToolSurface::CodexLike
                | BuiltinToolSurface::ClaudeCodeLike,
                BuiltinToolOperation::JobStart,
            ) => crate::environment::jobs::JOB_START_TOOL_NAME,
            (
                BuiltinToolSurface::Canonical
                | BuiltinToolSurface::CodexLike
                | BuiltinToolSurface::ClaudeCodeLike,
                BuiltinToolOperation::JobList,
            ) => crate::environment::jobs::JOB_LIST_TOOL_NAME,
            (
                BuiltinToolSurface::Canonical
                | BuiltinToolSurface::CodexLike
                | BuiltinToolSurface::ClaudeCodeLike,
                BuiltinToolOperation::JobRead,
            ) => crate::environment::jobs::JOB_READ_TOOL_NAME,
            (
                BuiltinToolSurface::Canonical
                | BuiltinToolSurface::CodexLike
                | BuiltinToolSurface::ClaudeCodeLike,
                BuiltinToolOperation::JobWait,
            ) => crate::environment::jobs::JOB_WAIT_TOOL_NAME,
            (
                BuiltinToolSurface::Canonical
                | BuiltinToolSurface::CodexLike
                | BuiltinToolSurface::ClaudeCodeLike,
                BuiltinToolOperation::JobCancel,
            ) => crate::environment::jobs::JOB_CANCEL_TOOL_NAME,
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::ReadFile) => "Read",
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::WriteFile) => "Write",
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::EditFile) => "Edit",
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::Grep) => "Grep",
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::Glob) => "Glob",
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::RunProcess) => "Bash",
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::ApplyPatch) => "apply_patch",
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::ListDir) => "list_dir",
            (BuiltinToolSurface::ClaudeCodeLike, BuiltinToolOperation::WriteProcessStdin) => {
                "write_stdin"
            }
        }
    }

    pub fn name(self, _target: &ToolTarget) -> ToolName {
        ToolName::new(self.name_str())
    }

    pub fn from_logical_id(logical_id: &str) -> Option<Self> {
        Some(match logical_id {
            "fs.read_file" | "host.read_file" => Self::canonical(BuiltinToolOperation::ReadFile),
            "fs.write_file" | "host.write_file" => Self::canonical(BuiltinToolOperation::WriteFile),
            "fs.edit_file" | "host.edit_file" => Self::canonical(BuiltinToolOperation::EditFile),
            "fs.apply_patch" | "host.apply_patch" => {
                Self::canonical(BuiltinToolOperation::ApplyPatch)
            }
            "fs.grep" | "host.grep" => Self::canonical(BuiltinToolOperation::Grep),
            "fs.glob" | "host.glob" => Self::canonical(BuiltinToolOperation::Glob),
            "fs.list_dir" | "host.list_dir" => Self::canonical(BuiltinToolOperation::ListDir),
            "env.run_process" | "host.run_process" => {
                Self::canonical(BuiltinToolOperation::RunProcess)
            }
            "env.write_process_stdin" | "host.write_process_stdin" => {
                Self::canonical(BuiltinToolOperation::WriteProcessStdin)
            }
            "env.job_start" | "host.job_start" => Self::canonical(BuiltinToolOperation::JobStart),
            "env.job_list" | "host.job_list" => Self::canonical(BuiltinToolOperation::JobList),
            "env.job_read" | "host.job_read" => Self::canonical(BuiltinToolOperation::JobRead),
            "env.job_wait" | "host.job_wait" => Self::canonical(BuiltinToolOperation::JobWait),
            "env.job_cancel" | "host.job_cancel" => {
                Self::canonical(BuiltinToolOperation::JobCancel)
            }
            "fs.codex.read_file" | "host.codex.read_file" => Self::new(
                BuiltinToolOperation::ReadFile,
                BuiltinToolSurface::CodexLike,
            ),
            "fs.codex.write_file" | "host.codex.write_file" => Self::new(
                BuiltinToolOperation::WriteFile,
                BuiltinToolSurface::CodexLike,
            ),
            "fs.codex.edit_file" | "host.codex.edit_file" => Self::new(
                BuiltinToolOperation::EditFile,
                BuiltinToolSurface::CodexLike,
            ),
            "fs.codex.apply_patch" | "host.codex.apply_patch" => Self::new(
                BuiltinToolOperation::ApplyPatch,
                BuiltinToolSurface::CodexLike,
            ),
            "fs.codex.grep" | "host.codex.grep" => {
                Self::new(BuiltinToolOperation::Grep, BuiltinToolSurface::CodexLike)
            }
            "fs.codex.glob" | "host.codex.glob" => {
                Self::new(BuiltinToolOperation::Glob, BuiltinToolSurface::CodexLike)
            }
            "fs.codex.list_dir" | "host.codex.list_dir" => {
                Self::new(BuiltinToolOperation::ListDir, BuiltinToolSurface::CodexLike)
            }
            "env.codex.run_process" | "host.codex.run_process" => Self::new(
                BuiltinToolOperation::RunProcess,
                BuiltinToolSurface::CodexLike,
            ),
            "env.codex.write_process_stdin" | "host.codex.write_process_stdin" => Self::new(
                BuiltinToolOperation::WriteProcessStdin,
                BuiltinToolSurface::CodexLike,
            ),
            "env.codex.job_start" | "host.codex.job_start" => Self::new(
                BuiltinToolOperation::JobStart,
                BuiltinToolSurface::CodexLike,
            ),
            "env.codex.job_list" | "host.codex.job_list" => {
                Self::new(BuiltinToolOperation::JobList, BuiltinToolSurface::CodexLike)
            }
            "env.codex.job_read" | "host.codex.job_read" => {
                Self::new(BuiltinToolOperation::JobRead, BuiltinToolSurface::CodexLike)
            }
            "env.codex.job_wait" | "host.codex.job_wait" => {
                Self::new(BuiltinToolOperation::JobWait, BuiltinToolSurface::CodexLike)
            }
            "env.codex.job_cancel" | "host.codex.job_cancel" => Self::new(
                BuiltinToolOperation::JobCancel,
                BuiltinToolSurface::CodexLike,
            ),
            "fs.claude.read_file" | "host.claude.read_file" => Self::new(
                BuiltinToolOperation::ReadFile,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "fs.claude.write_file" | "host.claude.write_file" => Self::new(
                BuiltinToolOperation::WriteFile,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "fs.claude.edit_file" | "host.claude.edit_file" => Self::new(
                BuiltinToolOperation::EditFile,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "fs.claude.apply_patch" | "host.claude.apply_patch" => Self::new(
                BuiltinToolOperation::ApplyPatch,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "fs.claude.grep" | "host.claude.grep" => Self::new(
                BuiltinToolOperation::Grep,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "fs.claude.glob" | "host.claude.glob" => Self::new(
                BuiltinToolOperation::Glob,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "fs.claude.list_dir" | "host.claude.list_dir" => Self::new(
                BuiltinToolOperation::ListDir,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "env.claude.run_process" | "host.claude.run_process" => Self::new(
                BuiltinToolOperation::RunProcess,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "env.claude.write_process_stdin" | "host.claude.write_process_stdin" => Self::new(
                BuiltinToolOperation::WriteProcessStdin,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "env.claude.job_start" | "host.claude.job_start" => Self::new(
                BuiltinToolOperation::JobStart,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "env.claude.job_list" | "host.claude.job_list" => Self::new(
                BuiltinToolOperation::JobList,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "env.claude.job_read" | "host.claude.job_read" => Self::new(
                BuiltinToolOperation::JobRead,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "env.claude.job_wait" | "host.claude.job_wait" => Self::new(
                BuiltinToolOperation::JobWait,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            "env.claude.job_cancel" | "host.claude.job_cancel" => Self::new(
                BuiltinToolOperation::JobCancel,
                BuiltinToolSurface::ClaudeCodeLike,
            ),
            _ => return None,
        })
    }

    pub const fn requires_write(self) -> bool {
        matches!(
            self.operation,
            BuiltinToolOperation::WriteFile
                | BuiltinToolOperation::EditFile
                | BuiltinToolOperation::ApplyPatch
        )
    }

    pub const fn requires_process(self) -> bool {
        matches!(
            self.operation,
            BuiltinToolOperation::RunProcess | BuiltinToolOperation::WriteProcessStdin
        )
    }

    pub const fn requires_jobs(self) -> bool {
        matches!(
            self.operation,
            BuiltinToolOperation::JobStart
                | BuiltinToolOperation::JobList
                | BuiltinToolOperation::JobRead
                | BuiltinToolOperation::JobWait
                | BuiltinToolOperation::JobCancel
        )
    }

    pub const fn is_filesystem_operation(self) -> bool {
        !self.requires_process() && !self.requires_jobs()
    }

    pub const fn target_namespace(self) -> &'static str {
        if self.is_filesystem_operation() {
            FS_TARGET_NAMESPACE
        } else {
            ENV_TARGET_NAMESPACE
        }
    }

    pub const fn parallelism(self) -> ToolParallelism {
        match self.operation {
            BuiltinToolOperation::ReadFile
            | BuiltinToolOperation::Grep
            | BuiltinToolOperation::Glob
            | BuiltinToolOperation::ListDir => ToolParallelism::ParallelSafe,
            BuiltinToolOperation::WriteFile
            | BuiltinToolOperation::EditFile
            | BuiltinToolOperation::ApplyPatch
            | BuiltinToolOperation::RunProcess
            | BuiltinToolOperation::WriteProcessStdin
            | BuiltinToolOperation::JobStart
            | BuiltinToolOperation::JobWait
            | BuiltinToolOperation::JobCancel => ToolParallelism::Exclusive,
            BuiltinToolOperation::JobList | BuiltinToolOperation::JobRead => {
                ToolParallelism::ParallelSafe
            }
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
                target_requirement: ToolTargetRequirement::required(self.target_namespace()),
            },
            documents: vec![description, input_schema],
        })
    }

    fn description(self, scoped_paths: bool) -> ToolResult<String> {
        match self.surface {
            BuiltinToolSurface::Canonical => {
                Ok(canonical::description(self.operation, scoped_paths))
            }
            BuiltinToolSurface::CodexLike => Ok(codex::description(self.operation, scoped_paths)),
            BuiltinToolSurface::ClaudeCodeLike => claude::description(self.operation, scoped_paths),
        }
    }

    fn input_schema(self, _target: &ToolTarget) -> ToolResult<Value> {
        match self.surface {
            BuiltinToolSurface::Canonical => Ok(canonical::input_schema(self.operation)),
            BuiltinToolSurface::CodexLike => Ok(codex::input_schema(self.operation)),
            BuiltinToolSurface::ClaudeCodeLike => claude::input_schema(self.operation),
        }
    }

    pub async fn invoke_json(
        self,
        ctx: ResolvedToolContext<'_>,
        arguments: Value,
    ) -> ToolResult<ToolInvocationOutput> {
        match self.surface {
            BuiltinToolSurface::Canonical => {
                canonical::invoke_json(self.operation, ctx, arguments).await
            }
            BuiltinToolSurface::CodexLike => {
                codex::invoke_json(self.operation, ctx, arguments).await
            }
            BuiltinToolSurface::ClaudeCodeLike => {
                claude::invoke_json(self.operation, ctx, arguments).await
            }
        }
    }
}

impl BuiltinToolOperation {
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
            Self::JobStart => "job_start",
            Self::JobList => "job_list",
            Self::JobRead => "job_read",
            Self::JobWait => "job_wait",
            Self::JobCancel => "job_cancel",
        }
    }
}

#[cfg(test)]
mod tests {
    use engine::{ProviderApiKind, ToolKind};
    use serde_json::json;

    use super::*;
    use crate::{fs::FsPath, runtime::decode_args, targets};

    fn target() -> ToolTarget {
        ToolTarget::api_kind(ProviderApiKind::OpenAiResponses)
    }

    #[test]
    fn built_in_tool_names_are_valid_tool_names() {
        for tool in [
            BuiltinTool::canonical(BuiltinToolOperation::ReadFile),
            BuiltinTool::canonical(BuiltinToolOperation::WriteFile),
            BuiltinTool::canonical(BuiltinToolOperation::EditFile),
            BuiltinTool::canonical(BuiltinToolOperation::ApplyPatch),
            BuiltinTool::canonical(BuiltinToolOperation::Grep),
            BuiltinTool::canonical(BuiltinToolOperation::Glob),
            BuiltinTool::canonical(BuiltinToolOperation::ListDir),
            BuiltinTool::canonical(BuiltinToolOperation::RunProcess),
            BuiltinTool::canonical(BuiltinToolOperation::WriteProcessStdin),
        ] {
            assert_eq!(tool.name(&target()).as_str(), tool.name_str());
        }
    }

    #[test]
    fn spec_bundle_uses_content_addressed_documents() {
        let bundle = BuiltinTool::canonical(BuiltinToolOperation::ReadFile)
            .spec_bundle(&target(), true)
            .expect("spec bundle");

        let ToolKind::Function(function) = bundle.spec.kind else {
            panic!("expected function tool");
        };
        assert_eq!(
            bundle.spec.target_requirement,
            ToolTargetRequirement::required(targets::FS_TARGET_NAMESPACE)
        );
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
    fn spec_bundle_routes_process_tools_to_environment_namespace() {
        let bundle = BuiltinTool::canonical(BuiltinToolOperation::RunProcess)
            .spec_bundle(&target(), true)
            .expect("spec bundle");

        assert_eq!(
            bundle.spec.target_requirement,
            ToolTargetRequirement::required(targets::ENV_TARGET_NAMESPACE)
        );
    }

    #[test]
    fn claude_code_like_surface_generates_claude_style_schema() {
        let tool = BuiltinTool::new(
            BuiltinToolOperation::ReadFile,
            BuiltinToolSurface::ClaudeCodeLike,
        );

        assert_eq!(tool.name_str(), "Read");
        let bundle = tool.spec_bundle(&target(), false).expect("spec bundle");
        assert!(bundle.documents[1].text_lossy().contains("\"file_path\""));
    }

    #[test]
    fn claude_code_like_surface_rejects_unmapped_operations() {
        let tool = BuiltinTool::new(
            BuiltinToolOperation::ApplyPatch,
            BuiltinToolSurface::ClaudeCodeLike,
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
