//! Provider-independent session tool registration.

use std::collections::BTreeMap;

use engine::{ProviderApiKind, ToolName, ToolSpec, WorkflowToolBinding};

use crate::{
    builtin::{BuiltinTool, BuiltinToolDomain, BuiltinToolOperation, BuiltinToolSurface},
    concurrency::ConcurrencyToolsetConfig,
    definitions::{BuiltinSettings, register},
    error::{ToolError, ToolResult},
    runtime::ToolTarget,
    web::search::WebSearchToolConfig,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolsetConfig {
    pub builtin: BuiltinToolsetConfig,
    pub web: WebToolsetConfig,
    pub concurrency: ConcurrencyToolsetConfig,
    pub environment_read: bool,
    pub environment_selection: bool,
}

impl ToolsetConfig {
    pub fn empty() -> Self {
        Self {
            builtin: BuiltinToolsetConfig::disabled(),
            web: WebToolsetConfig::default(),
            concurrency: ConcurrencyToolsetConfig::default(),
            environment_read: false,
            environment_selection: false,
        }
    }

    pub fn workspace() -> Self {
        Self {
            builtin: BuiltinToolsetConfig::workspace(),
            ..Self::empty()
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WebToolsetConfig {
    pub search: Option<WebSearchToolConfig>,
    pub fetch: bool,
}

impl Default for ToolsetConfig {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltinToolsetConfig {
    pub presentation: BuiltinToolPresentation,
    pub vfs: FilesystemToolsetConfig,
    pub environment: EnvironmentToolsetConfig,
}

impl BuiltinToolsetConfig {
    pub fn disabled() -> Self {
        Self {
            presentation: BuiltinToolPresentation::ProviderDefault,
            vfs: FilesystemToolsetConfig::disabled(),
            environment: EnvironmentToolsetConfig::disabled(),
        }
    }

    pub fn workspace() -> Self {
        Self {
            vfs: FilesystemToolsetConfig::workspace_edit(),
            ..Self::disabled()
        }
    }

    pub fn from_operations(operations: impl IntoIterator<Item = BuiltinToolOperation>) -> Self {
        let mut config = Self::disabled();
        for operation in operations {
            config.enable_operation(operation);
        }
        config
    }

    pub fn vfs_from_operations(
        operations: impl IntoIterator<Item = BuiltinToolOperation>,
    ) -> ToolResult<Self> {
        let mut config = Self::disabled();
        for operation in operations {
            match operation {
                BuiltinToolOperation::ReadFile => config.vfs.read_file = true,
                BuiltinToolOperation::WriteFile => config.vfs.write_file = true,
                BuiltinToolOperation::EditFile => config.vfs.edit_file = true,
                BuiltinToolOperation::ApplyPatch => config.vfs.apply_patch = true,
                BuiltinToolOperation::Grep => config.vfs.grep = true,
                BuiltinToolOperation::Glob => config.vfs.glob = true,
                BuiltinToolOperation::ListDir => config.vfs.list_dir = true,
                BuiltinToolOperation::Materialize | BuiltinToolOperation::Capture => {
                    return Err(ToolError::InvalidRequest {
                        message: "transfer tools require both VFS and environment grants".into(),
                    });
                }
                BuiltinToolOperation::RunProcess
                | BuiltinToolOperation::ContinueProcess
                | BuiltinToolOperation::JobSubmit
                | BuiltinToolOperation::JobRun
                | BuiltinToolOperation::JobRead => {
                    return Err(ToolError::InvalidRequest {
                        message: "non-filesystem operation cannot be enabled in the VFS domain"
                            .to_owned(),
                    });
                }
            }
        }
        Ok(config)
    }

    pub fn enable_operation(&mut self, operation: BuiltinToolOperation) {
        match operation {
            BuiltinToolOperation::Materialize => {
                self.vfs.read_file = true;
                self.environment.filesystem.write_file = true;
            }
            BuiltinToolOperation::Capture => {
                self.vfs.write_file = true;
                self.environment.filesystem.read_file = true;
            }
            BuiltinToolOperation::ReadFile => self.environment.filesystem.read_file = true,
            BuiltinToolOperation::WriteFile => self.environment.filesystem.write_file = true,
            BuiltinToolOperation::EditFile => self.environment.filesystem.edit_file = true,
            BuiltinToolOperation::ApplyPatch => self.environment.filesystem.apply_patch = true,
            BuiltinToolOperation::Grep => self.environment.filesystem.grep = true,
            BuiltinToolOperation::Glob => self.environment.filesystem.glob = true,
            BuiltinToolOperation::ListDir => self.environment.filesystem.list_dir = true,
            BuiltinToolOperation::RunProcess => self.environment.run_process = true,
            BuiltinToolOperation::ContinueProcess => self.environment.continue_process = true,
            BuiltinToolOperation::JobSubmit => self.environment.job_submit = true,
            BuiltinToolOperation::JobRun => self.environment.job_run = true,
            BuiltinToolOperation::JobRead => self.environment.job_read = true,
        }
    }

    pub fn enabled(&self) -> bool {
        self.vfs.enabled() || self.environment.enabled()
    }

    /// Compose the VFS family from its filesystem grants and the environment
    /// grants needed for explicit transfers between the two domains.
    fn vfs_operations(&self) -> Vec<BuiltinToolOperation> {
        let mut operations = self.vfs.operations();
        if self.vfs.read_file && self.environment.filesystem.write_file {
            operations.push(BuiltinToolOperation::Materialize);
        }
        if self.vfs.write_file && self.environment.filesystem.read_file {
            operations.push(BuiltinToolOperation::Capture);
        }
        operations
    }
}

impl Default for BuiltinToolsetConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinToolPresentation {
    #[default]
    ProviderDefault,
    Canonical,
    CodexLike,
    ClaudeCodeLike,
}

impl BuiltinToolPresentation {
    /// Responses and Completions share neutral filesystem tools and shell
    /// command execution. The resolver omits patch tools from the generic
    /// Completions default; explicit presentations retain their full surface.
    pub(crate) fn surface(self, target: &ToolTarget) -> BuiltinToolSurface {
        match self {
            Self::ProviderDefault => match target.api_kind {
                ProviderApiKind::AnthropicMessages => BuiltinToolSurface::ClaudeCodeLike,
                ProviderApiKind::OpenAiResponses | ProviderApiKind::OpenAiCompletions => {
                    BuiltinToolSurface::CodexLike
                }
            },
            Self::Canonical => BuiltinToolSurface::Canonical,
            Self::CodexLike => BuiltinToolSurface::CodexLike,
            Self::ClaudeCodeLike => BuiltinToolSurface::ClaudeCodeLike,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FilesystemToolsetConfig {
    pub read_file: bool,
    pub write_file: bool,
    pub edit_file: bool,
    pub apply_patch: bool,
    pub grep: bool,
    pub glob: bool,
    pub list_dir: bool,
}

impl FilesystemToolsetConfig {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn read_only() -> Self {
        Self {
            read_file: true,
            grep: true,
            glob: true,
            list_dir: true,
            ..Self::disabled()
        }
    }

    pub fn workspace_edit() -> Self {
        Self {
            write_file: true,
            edit_file: true,
            apply_patch: true,
            ..Self::read_only()
        }
    }

    pub fn enabled(&self) -> bool {
        self.read_file
            || self.write_file
            || self.edit_file
            || self.apply_patch
            || self.grep
            || self.glob
            || self.list_dir
    }

    fn operations(&self) -> Vec<BuiltinToolOperation> {
        let mut operations = Vec::new();
        if self.read_file {
            operations.push(BuiltinToolOperation::ReadFile);
        }
        if self.write_file {
            operations.push(BuiltinToolOperation::WriteFile);
        }
        if self.edit_file {
            operations.push(BuiltinToolOperation::EditFile);
        }
        if self.apply_patch {
            operations.push(BuiltinToolOperation::ApplyPatch);
        }
        if self.grep {
            operations.push(BuiltinToolOperation::Grep);
        }
        if self.glob {
            operations.push(BuiltinToolOperation::Glob);
        }
        if self.list_dir {
            operations.push(BuiltinToolOperation::ListDir);
        }
        operations
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnvironmentToolsetConfig {
    pub filesystem: FilesystemToolsetConfig,
    pub run_process: bool,
    /// The handle path. When off, every surface renders its one-shot
    /// process shape: the run tool waits to exit and no continue tool exists.
    pub continue_process: bool,
    pub job_submit: bool,
    pub job_run: bool,
    pub job_read: bool,
}

impl EnvironmentToolsetConfig {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn basic() -> Self {
        Self {
            filesystem: FilesystemToolsetConfig::workspace_edit(),
            run_process: true,
            continue_process: true,
            ..Self::disabled()
        }
    }

    pub fn jobs() -> Self {
        Self {
            job_submit: true,
            job_read: true,
            ..Self::disabled()
        }
    }

    pub fn with_jobs(mut self) -> Self {
        self.job_submit = true;
        self.job_read = true;
        self
    }

    pub fn enabled(&self) -> bool {
        self.filesystem.enabled()
            || self.run_process
            || self.continue_process
            || self.job_submit
            || self.job_run
            || self.job_read
    }

    pub fn jobs_enabled(&self) -> bool {
        self.job_submit || self.job_run || self.job_read
    }

    fn operations(&self) -> Vec<BuiltinToolOperation> {
        let mut operations = self.filesystem.operations();
        if self.run_process {
            operations.push(BuiltinToolOperation::RunProcess);
        }
        if self.run_process && self.continue_process {
            operations.push(BuiltinToolOperation::ContinueProcess);
        }
        if self.job_submit {
            operations.push(BuiltinToolOperation::JobSubmit);
        }
        if self.job_run {
            operations.push(BuiltinToolOperation::JobRun);
        }
        if self.job_read {
            operations.push(BuiltinToolOperation::JobRead);
        }
        operations
    }
}

/// Admitted logical registrations. Provider presentation is resolved per turn.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RegisteredToolset {
    pub tools: BTreeMap<ToolName, ToolSpec>,
}

/// Explicit-Promise workflow tools create model-owned completion promises, so their
/// admission pulls concurrency tools into the model toolset. Joined tools use
/// runtime-owned Promises and must not grant await/cancel/detach.
pub fn enable_concurrency_for_workflow_tools<'a>(
    config: &mut ToolsetConfig,
    bindings: impl IntoIterator<Item = &'a WorkflowToolBinding>,
) {
    if bindings
        .into_iter()
        .any(|binding| binding.completion.exposes_model_owned_promises())
    {
        config.concurrency.enabled = true;
    }
}

/// Merge trusted, already-admitted workflow definitions into the registry.
pub fn register_workflow_tools<'a>(
    toolset: &mut RegisteredToolset,
    bindings: impl IntoIterator<Item = &'a WorkflowToolBinding>,
) -> ToolResult<()> {
    for binding in bindings {
        binding
            .validate()
            .map_err(|error| ToolError::InvalidRequest {
                message: format!(
                    "invalid workflow tool {} binding: {error}",
                    binding.definition.tool_id
                ),
            })?;
        let tool_name = binding.definition.tool.name.clone();
        if toolset.tools.contains_key(&tool_name) {
            return Err(ToolError::InvalidRequest {
                message: format!(
                    "workflow tool {} tool name {} collides with another effective tool",
                    binding.definition.tool_id, tool_name
                ),
            });
        }
        toolset
            .tools
            .insert(tool_name, binding.definition.tool.clone());
    }
    Ok(())
}

pub fn register_toolset(config: &ToolsetConfig) -> ToolResult<RegisteredToolset> {
    use engine::{ToolExecutionClass, ToolExecutionSpec, ToolParallelism};
    let mut tools = BTreeMap::new();
    let mut add = |tool: ToolSpec| -> ToolResult<()> {
        let name = tool.name.clone();
        if tools.insert(name.clone(), tool).is_some() {
            return Err(ToolError::InvalidRequest {
                message: format!("duplicate tool registration {name}"),
            });
        }
        Ok(())
    };
    for (domain, operations) in [
        (BuiltinToolDomain::Vfs, config.builtin.vfs_operations()),
        (
            BuiltinToolDomain::Environment,
            config.builtin.environment.operations(),
        ),
    ] {
        for operation in operations {
            let tool = match domain {
                BuiltinToolDomain::Vfs => {
                    BuiltinTool::vfs(operation, BuiltinToolSurface::Canonical)
                }
                BuiltinToolDomain::Environment => {
                    BuiltinTool::environment(operation, BuiltinToolSurface::Canonical)
                }
            };
            add(register(
                tool.logical_id(),
                BuiltinSettings {
                    presentation: config.builtin.presentation,
                    one_shot: operation == BuiltinToolOperation::RunProcess
                        && !config.builtin.environment.continue_process,
                    ..Default::default()
                },
                tool.parallelism(),
                tool.execution_spec(),
            ))?;
        }
    }
    if let Some(search) = &config.web.search {
        add(register(
            "web.search",
            BuiltinSettings {
                allowed_domains: search.allowed_domains.clone(),
                blocked_domains: search.blocked_domains.clone(),
                ..Default::default()
            },
            ToolParallelism::ParallelSafe,
            ToolExecutionSpec::default(),
        ))?;
    }
    if config.web.fetch {
        add(register(
            "web.fetch",
            BuiltinSettings::default(),
            ToolParallelism::ParallelSafe,
            ToolExecutionSpec::new(ToolExecutionClass::RemoteInteractive, true),
        ))?;
    }
    if config.environment_read || config.environment_selection {
        add(register(
            "environment.read",
            BuiltinSettings::default(),
            ToolParallelism::ParallelSafe,
            ToolExecutionSpec::new(ToolExecutionClass::RemoteInteractive, true),
        ))?;
    }
    if config.environment_selection {
        for (id, parallelism, retry_safe) in [
            ("environment.list", ToolParallelism::ParallelSafe, true),
            ("environment.activate", ToolParallelism::Exclusive, false),
            ("environment.deactivate", ToolParallelism::Exclusive, false),
        ] {
            add(register(
                id,
                BuiltinSettings::default(),
                parallelism,
                ToolExecutionSpec::new(ToolExecutionClass::RemoteInteractive, retry_safe),
            ))?;
        }
    }
    if config.concurrency.enabled_or_timer() || config.builtin.environment.jobs_enabled() {
        for id in [
            "concurrency.await",
            "concurrency.cancel",
            "concurrency.detach",
        ] {
            add(register(
                id,
                BuiltinSettings::default(),
                ToolParallelism::Exclusive,
                ToolExecutionSpec::default(),
            ))?;
        }
        if config.concurrency.timer {
            add(register(
                "concurrency.sleep",
                BuiltinSettings::default(),
                ToolParallelism::Exclusive,
                ToolExecutionSpec::default(),
            ))?;
        }
    }
    Ok(RegisteredToolset { tools })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::concurrency::{AWAIT_TOOL_NAME, CANCEL_TOOL_NAME, SLEEP_TOOL_NAME};
    use crate::subagents::AGENT_SPAWN_TOOL_NAME;
    use crate::web::fetch::WEB_FETCH_TOOL_NAME;
    use crate::web::search::WebSearchToolConfig;

    use crate::definitions::{Definition, ResolvedBuiltin};
    use crate::runtime::ToolCatalog;

    struct PresentedToolset {
        tools: BTreeMap<ToolName, ResolvedBuiltin>,
        catalog: ToolCatalog,
    }

    fn present_toolset(
        target: &ToolTarget,
        config: &ToolsetConfig,
    ) -> ToolResult<PresentedToolset> {
        let registry = super::register_toolset(config)?;
        let mut tools = BTreeMap::new();
        for tool in registry.tools.values() {
            let engine::ToolKind::Builtin(spec) = &tool.kind else {
                panic!("built-in registration");
            };
            for resolved in crate::definitions::resolve(&tool.name, spec, target)? {
                assert!(tools.insert(resolved.name.clone(), resolved).is_none());
            }
        }
        Ok(PresentedToolset {
            tools,
            catalog: ToolCatalog::from_registrations(&registry.tools, target)?,
        })
    }

    fn target(api_kind: ProviderApiKind) -> ToolTarget {
        ToolTarget::api_kind(api_kind)
    }

    fn visible_names(toolset: &PresentedToolset) -> Vec<String> {
        toolset
            .tools
            .keys()
            .map(|name| name.as_str().to_owned())
            .collect()
    }

    fn input_schema(toolset: &PresentedToolset, name: &str) -> Value {
        let spec = toolset
            .tools
            .get(&ToolName::new(name))
            .unwrap_or_else(|| panic!("tool {name} is listed"));
        let Definition::Function(function) = &spec.definition else {
            panic!("function definition");
        };
        function.input_schema.clone()
    }

    fn native_definition(toolset: &PresentedToolset) -> Value {
        let Definition::Native(native) = &toolset.tools.values().next().unwrap().definition else {
            panic!("native definition");
        };
        native.clone()
    }

    fn property_names(schema: &Value) -> Vec<String> {
        schema["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .cloned()
            .collect()
    }

    fn process_config() -> ToolsetConfig {
        let mut config = ToolsetConfig::empty();
        config.builtin.environment = EnvironmentToolsetConfig {
            run_process: true,
            continue_process: true,
            ..EnvironmentToolsetConfig::disabled()
        };
        config
    }

    #[test]
    fn provider_defaults_use_shell_commands_and_keep_explicit_canonical_execution() {
        let mut config = process_config();

        let responses = target(ProviderApiKind::OpenAiResponses);
        let toolset = present_toolset(&responses, &config).expect("toolset");
        assert_eq!(visible_names(&toolset), vec!["exec_command", "write_stdin"]);
        let exec = input_schema(&toolset, "exec_command");
        assert_eq!(
            property_names(&exec),
            [
                "cmd",
                "login",
                "max_output_tokens",
                "tty",
                "workdir",
                "yield_time_ms"
            ]
        );
        assert_eq!(exec["required"], json!(["cmd"]));
        let write = input_schema(&toolset, "write_stdin");
        assert_eq!(
            property_names(&write),
            ["chars", "max_output_tokens", "session_id", "yield_time_ms"]
        );
        assert_eq!(
            toolset
                .catalog
                .get(&ToolName::new("write_stdin"))
                .expect("binding")
                .logical_id,
            "env.continue_process"
        );

        let anthropic = target(ProviderApiKind::AnthropicMessages);
        let toolset = present_toolset(&anthropic, &config).expect("toolset");
        assert_eq!(
            visible_names(&toolset),
            vec!["Bash", "BashOutput", "KillShell"]
        );
        assert_eq!(
            property_names(&input_schema(&toolset, "Bash")),
            [
                "command",
                "dangerouslyDisableSandbox",
                "description",
                "run_in_background",
                "timeout"
            ]
        );
        assert_eq!(
            property_names(&input_schema(&toolset, "BashOutput")),
            ["bash_id", "filter", "timeout"]
        );
        assert_eq!(
            property_names(&input_schema(&toolset, "KillShell")),
            ["shell_id"]
        );
        for name in ["BashOutput", "KillShell"] {
            assert_eq!(
                toolset
                    .catalog
                    .get(&ToolName::new(name))
                    .expect("binding")
                    .logical_id,
                "env.continue_process"
            );
        }

        let completions = target(ProviderApiKind::OpenAiCompletions);
        let toolset = present_toolset(&completions, &config).expect("toolset");
        assert_eq!(visible_names(&toolset), vec!["exec_command", "write_stdin"]);
        assert_eq!(
            input_schema(&toolset, "exec_command"),
            input_schema(
                &present_toolset(&responses, &config).unwrap(),
                "exec_command"
            )
        );
        assert_eq!(
            input_schema(&toolset, "write_stdin"),
            input_schema(
                &present_toolset(&responses, &config).unwrap(),
                "write_stdin"
            )
        );

        config.builtin.presentation = BuiltinToolPresentation::Canonical;
        let toolset = present_toolset(&completions, &config).expect("explicit canonical tools");
        assert_eq!(
            visible_names(&toolset),
            vec!["continue_process", "run_process"]
        );
        let run = input_schema(&toolset, "run_process");
        assert_eq!(
            property_names(&run),
            [
                "argv",
                "cwd",
                "env",
                "max_output_bytes",
                "stdin",
                "timeout_ms",
                "tty",
                "yield_ms"
            ]
        );
        assert_eq!(
            property_names(&input_schema(&toolset, "continue_process")),
            [
                "close_stdin",
                "handle",
                "input",
                "max_output_bytes",
                "signal",
                "wait_ms"
            ]
        );
    }

    #[test]
    fn completions_patch_tools_require_an_explicit_presentation() {
        let target = target(ProviderApiKind::OpenAiCompletions);
        let mut config = ToolsetConfig::workspace();
        config.builtin.environment = EnvironmentToolsetConfig::basic();
        let names = visible_names(&present_toolset(&target, &config).unwrap());
        for name in [
            "read_file",
            "write_file",
            "edit_file",
            "vfs_read_file",
            "vfs_write_file",
            "vfs_edit_file",
        ] {
            assert!(names.contains(&name.to_owned()), "missing {name}");
        }
        for name in [
            "apply_patch",
            "vfs_apply_patch",
            "run_process",
            "continue_process",
        ] {
            assert!(!names.contains(&name.to_owned()), "unexpected {name}");
        }

        for presentation in [
            BuiltinToolPresentation::CodexLike,
            BuiltinToolPresentation::Canonical,
        ] {
            config.builtin.presentation = presentation;
            let names = visible_names(&present_toolset(&target, &config).unwrap());
            assert!(names.contains(&"apply_patch".to_owned()));
            assert!(names.contains(&"vfs_apply_patch".to_owned()));
        }

        config.builtin.vfs.apply_patch = false;
        config.builtin.environment.filesystem.apply_patch = false;
        let names = visible_names(&present_toolset(&target, &config).unwrap());
        assert!(!names.contains(&"apply_patch".to_owned()));
        assert!(!names.contains(&"vfs_apply_patch".to_owned()));
    }

    #[test]
    fn one_shot_policy_renders_the_restricted_shape_on_every_surface() {
        let mut config = process_config();
        config.builtin.environment.continue_process = false;

        let responses = target(ProviderApiKind::OpenAiResponses);
        let toolset = present_toolset(&responses, &config).expect("toolset");
        assert_eq!(visible_names(&toolset), vec!["exec_command"]);
        assert_eq!(
            property_names(&input_schema(&toolset, "exec_command")),
            ["cmd", "login", "max_output_tokens", "timeout_ms", "workdir"]
        );
        assert_eq!(
            toolset
                .catalog
                .get(&ToolName::new("exec_command"))
                .expect("binding")
                .adapter_id
                .as_deref(),
            Some("codex-oneshot")
        );

        let anthropic = target(ProviderApiKind::AnthropicMessages);
        let toolset = present_toolset(&anthropic, &config).expect("toolset");
        assert_eq!(visible_names(&toolset), vec!["Bash"]);
        assert!(
            !property_names(&input_schema(&toolset, "Bash"))
                .contains(&"run_in_background".to_owned())
        );

        let completions = target(ProviderApiKind::OpenAiCompletions);
        let toolset = present_toolset(&completions, &config).expect("toolset");
        assert_eq!(visible_names(&toolset), vec!["exec_command"]);
        assert!(
            !property_names(&input_schema(&toolset, "exec_command"))
                .contains(&"yield_time_ms".to_owned())
        );
    }

    #[test]
    fn workspace_toolset_renders_openai_canonical_builtin_tools() {
        let target = target(ProviderApiKind::OpenAiResponses);

        let toolset = present_toolset(&target, &ToolsetConfig::workspace()).expect("toolset");

        assert_eq!(
            visible_names(&toolset),
            vec![
                "vfs_apply_patch",
                "vfs_edit_file",
                "vfs_glob",
                "vfs_grep",
                "vfs_list_dir",
                "vfs_read_file",
                "vfs_write_file"
            ]
        );
        assert!(
            toolset
                .catalog
                .get(&ToolName::new("vfs_read_file"))
                .is_some()
        );
    }

    #[test]
    fn read_only_vfs_toolset_exposes_only_four_vfs_tools() {
        let target = target(ProviderApiKind::OpenAiResponses);
        let mut config = ToolsetConfig::empty();
        config.builtin.vfs = FilesystemToolsetConfig::read_only();

        let toolset = present_toolset(&target, &config).expect("toolset");

        assert_eq!(
            visible_names(&toolset),
            vec!["vfs_glob", "vfs_grep", "vfs_list_dir", "vfs_read_file"]
        );
        assert!(
            toolset
                .catalog
                .bindings()
                .all(|binding| binding.logical_id.starts_with("vfs."))
        );
    }

    #[test]
    fn environment_and_vfs_toolsets_are_non_colliding() {
        let target = target(ProviderApiKind::OpenAiResponses);
        let mut config = ToolsetConfig::workspace();
        config.builtin.environment = EnvironmentToolsetConfig::basic();

        let toolset = present_toolset(&target, &config).expect("toolset");
        let names = visible_names(&toolset);

        assert!(names.contains(&"vfs_read_file".to_owned()));
        assert!(names.contains(&"read_file".to_owned()));
        assert!(names.contains(&"exec_command".to_owned()));
        assert!(names.contains(&"vfs_materialize".to_owned()));
        assert!(names.contains(&"vfs_capture".to_owned()));
        assert_eq!(names.len(), 18);
        assert!(
            toolset
                .catalog
                .bindings()
                .any(|binding| binding.logical_id == "vfs.read_file")
        );
        assert!(
            toolset
                .catalog
                .bindings()
                .any(|binding| binding.logical_id == "env.read_file")
        );
    }

    #[test]
    fn job_toolset_adds_suspension_tools_without_subagent_tools() {
        let target = target(ProviderApiKind::OpenAiResponses);
        let mut config = ToolsetConfig::empty();
        config.builtin.environment = EnvironmentToolsetConfig::jobs();

        let toolset = present_toolset(&target, &config).expect("toolset");
        let names = visible_names(&toolset);

        assert!(names.contains(&"job_submit".to_owned()));
        assert!(names.contains(&"job_read".to_owned()));
        assert!(names.contains(&AWAIT_TOOL_NAME.to_owned()));
        assert!(names.contains(&CANCEL_TOOL_NAME.to_owned()));
        assert!(!names.contains(&AGENT_SPAWN_TOOL_NAME.to_owned()));
        assert!(
            toolset
                .catalog
                .get(&ToolName::new(AWAIT_TOOL_NAME))
                .is_some()
        );
        assert!(
            toolset
                .catalog
                .get(&ToolName::new(CANCEL_TOOL_NAME))
                .is_some()
        );
    }

    #[test]
    fn environment_read_is_independent_from_default_off_selection_tools() {
        let target = target(ProviderApiKind::OpenAiResponses);
        let disabled = present_toolset(&target, &ToolsetConfig::empty()).expect("disabled toolset");
        assert!(
            visible_names(&disabled)
                .iter()
                .all(|name| !name.starts_with("environment_"))
        );

        let mut config = ToolsetConfig::empty();
        config.environment_read = true;
        let read_only = present_toolset(&target, &config).expect("environment read toolset");
        assert_eq!(visible_names(&read_only), vec!["environment_read"]);

        config.environment_selection = true;
        let enabled = present_toolset(&target, &config).expect("selection toolset");

        assert_eq!(
            visible_names(&enabled),
            vec![
                "environment_activate",
                "environment_deactivate",
                "environment_list",
                "environment_read",
            ]
        );
    }

    #[test]
    fn timer_toolset_adds_sleep_and_concurrency_tools() {
        let target = target(ProviderApiKind::OpenAiResponses);
        let mut config = ToolsetConfig::empty();
        config.concurrency = ConcurrencyToolsetConfig::timer();

        let toolset = present_toolset(&target, &config).expect("toolset");
        let names = visible_names(&toolset);

        assert!(names.contains(&AWAIT_TOOL_NAME.to_owned()));
        assert!(names.contains(&CANCEL_TOOL_NAME.to_owned()));
        assert!(names.contains(&SLEEP_TOOL_NAME.to_owned()));
        assert!(!names.contains(&AGENT_SPAWN_TOOL_NAME.to_owned()));
    }

    #[test]
    fn builtin_tool_presentation_defaults_to_claude_style_for_anthropic() {
        let target = target(ProviderApiKind::AnthropicMessages);
        let mut config = ToolsetConfig::empty();
        config.builtin = BuiltinToolsetConfig {
            vfs: FilesystemToolsetConfig {
                read_file: true,
                ..FilesystemToolsetConfig::disabled()
            },
            ..BuiltinToolsetConfig::disabled()
        };

        let toolset = present_toolset(&target, &config).expect("toolset");

        assert_eq!(visible_names(&toolset), vec!["VfsRead"]);
        assert!(
            input_schema(&toolset, "VfsRead")["properties"]
                .get("file_path")
                .is_some()
        );
    }

    #[test]
    fn workspace_provider_default_omits_builtin_tools_unsupported_by_provider_surface() {
        let target = target(ProviderApiKind::AnthropicMessages);

        let toolset = present_toolset(&target, &ToolsetConfig::workspace()).expect("toolset");

        assert_eq!(
            visible_names(&toolset),
            vec![
                "VfsEdit",
                "VfsGlob",
                "VfsGrep",
                "VfsListDir",
                "VfsRead",
                "VfsWrite"
            ]
        );
    }

    #[test]
    fn anthropic_list_dir_names_preserve_vfs_and_environment_domains() {
        let target = target(ProviderApiKind::AnthropicMessages);
        let mut config = ToolsetConfig::empty();
        config.builtin.vfs.list_dir = true;
        config.builtin.environment.filesystem.list_dir = true;

        let toolset = present_toolset(&target, &config).expect("toolset");

        assert_eq!(visible_names(&toolset), vec!["ListDir", "VfsListDir"]);
        assert_eq!(
            toolset
                .catalog
                .get(&ToolName::new("ListDir"))
                .expect("environment list binding")
                .logical_id,
            "env.list_dir"
        );
        assert_eq!(
            toolset
                .catalog
                .get(&ToolName::new("VfsListDir"))
                .expect("VFS list binding")
                .logical_id,
            "vfs.list_dir"
        );
    }

    #[test]
    fn web_search_adds_provider_native_tool_and_defaults_patch() {
        let target = target(ProviderApiKind::OpenAiResponses);
        let mut config = ToolsetConfig::empty();
        config.web.search = Some(WebSearchToolConfig::new(
            vec!["docs.rs".to_owned()],
            Vec::new(),
        ));

        let toolset = present_toolset(&target, &config).expect("toolset");

        assert_eq!(visible_names(&toolset), vec!["web_search"]);
        assert!(toolset.catalog.is_empty());
        let native = native_definition(&toolset);
        assert_eq!(
            native,
            json!({
                "type": "web_search",
                "external_web_access": false,
                "filters": { "allowed_domains": ["docs.rs"] }
            })
        );
    }

    #[test]
    fn web_search_uses_anthropic_hosted_tool() {
        let target = target(ProviderApiKind::AnthropicMessages);
        let mut config = ToolsetConfig::empty();
        config.web.search = Some(WebSearchToolConfig::default());

        let toolset = present_toolset(&target, &config).expect("toolset");

        assert_eq!(visible_names(&toolset), vec!["web_search"]);
        assert!(toolset.catalog.is_empty());
        let native = native_definition(&toolset);
        assert_eq!(native["type"], json!("web_search_20250305"));
    }

    #[test]
    fn web_fetch_adds_standard_function_tool_and_catalog_binding() {
        let target = target(ProviderApiKind::OpenAiResponses);
        let mut config = ToolsetConfig::empty();
        config.web.fetch = true;

        let toolset = present_toolset(&target, &config).expect("toolset");

        assert_eq!(visible_names(&toolset), vec![WEB_FETCH_TOOL_NAME]);
        assert!(
            toolset
                .catalog
                .get(&ToolName::new(WEB_FETCH_TOOL_NAME))
                .is_some()
        );
        let _spec = toolset
            .tools
            .get(&ToolName::new(WEB_FETCH_TOOL_NAME))
            .expect("web_fetch spec");
    }

    #[test]
    fn web_fetch_uses_anthropic_hosted_tool_without_local_binding() {
        let target = target(ProviderApiKind::AnthropicMessages);
        let mut config = ToolsetConfig::empty();
        config.web.fetch = true;

        let toolset = present_toolset(&target, &config).expect("toolset");

        assert_eq!(visible_names(&toolset), vec![WEB_FETCH_TOOL_NAME]);
        assert!(toolset.catalog.is_empty());
        let spec = toolset
            .tools
            .get(&ToolName::new(WEB_FETCH_TOOL_NAME))
            .expect("web_fetch spec");
        assert!(matches!(spec.definition, Definition::Native(_)));
    }
}
