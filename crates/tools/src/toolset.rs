//! Provider-aware session toolset composition.

use std::collections::{BTreeMap, BTreeSet};

use engine::{ProviderApiKind, ToolName, ToolSpec};

use crate::{
    builtin::{BuiltinTool, BuiltinToolOperation, BuiltinToolSurface},
    error::{ToolError, ToolResult},
    fleet::{FleetToolsetConfig, fleet_tool_bindings, fleet_tool_bundles},
    messaging::{MessagingToolsetConfig, messaging_tool_bindings, messaging_tool_bundles},
    runtime::{ToolCatalog, ToolDocument, ToolExecutionMode, ToolSpecBundle, ToolTarget},
    web::fetch::{WebFetchToolConfig, web_fetch_tool_binding, web_fetch_tool_bundle},
    web::search::{
        OpenAiResponsesWebSearchConfig, apply_openai_responses_web_search_includes,
        openai_responses_web_search_tool_bundle,
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolsetConfig {
    pub builtin: BuiltinToolsetConfig,
    pub openai_web_search: OpenAiResponsesWebSearchConfig,
    pub web_fetch: WebFetchToolConfig,
    pub messaging: MessagingToolsetConfig,
    pub fleet: FleetToolsetConfig,
}

impl ToolsetConfig {
    pub fn empty() -> Self {
        Self {
            builtin: BuiltinToolsetConfig::disabled(),
            openai_web_search: OpenAiResponsesWebSearchConfig::default(),
            web_fetch: WebFetchToolConfig::default(),
            messaging: MessagingToolsetConfig::default(),
            fleet: FleetToolsetConfig::default(),
        }
    }

    pub fn workspace() -> Self {
        Self {
            builtin: BuiltinToolsetConfig::workspace(),
            ..Self::empty()
        }
    }
}

impl Default for ToolsetConfig {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltinToolsetConfig {
    pub presentation: BuiltinToolPresentation,
    pub fs: FilesystemToolsetConfig,
    pub process: EnvironmentToolsetConfig,
    pub execution: ToolExecutionMode,
}

impl BuiltinToolsetConfig {
    pub fn disabled() -> Self {
        Self {
            presentation: BuiltinToolPresentation::ProviderDefault,
            fs: FilesystemToolsetConfig::disabled(),
            process: EnvironmentToolsetConfig::disabled(),
            execution: ToolExecutionMode::Inline,
        }
    }

    pub fn workspace() -> Self {
        Self {
            fs: FilesystemToolsetConfig::workspace_edit(),
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

    pub fn enable_operation(&mut self, operation: BuiltinToolOperation) {
        match operation {
            BuiltinToolOperation::ReadFile => self.fs.read_file = true,
            BuiltinToolOperation::WriteFile => self.fs.write_file = true,
            BuiltinToolOperation::EditFile => self.fs.edit_file = true,
            BuiltinToolOperation::ApplyPatch => self.fs.apply_patch = true,
            BuiltinToolOperation::Grep => self.fs.grep = true,
            BuiltinToolOperation::Glob => self.fs.glob = true,
            BuiltinToolOperation::ListDir => self.fs.list_dir = true,
            BuiltinToolOperation::RunProcess => self.process.run_process = true,
            BuiltinToolOperation::WriteProcessStdin => self.process.write_process_stdin = true,
            BuiltinToolOperation::JobStart => self.process.job_start = true,
            BuiltinToolOperation::JobList => self.process.job_list = true,
            BuiltinToolOperation::JobRead => self.process.job_read = true,
            BuiltinToolOperation::JobWait => self.process.job_wait = true,
            BuiltinToolOperation::JobCancel => self.process.job_cancel = true,
        }
    }

    pub fn enabled(&self) -> bool {
        self.fs.enabled() || self.process.enabled()
    }
}

impl Default for BuiltinToolsetConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BuiltinToolPresentation {
    #[default]
    ProviderDefault,
    Canonical,
    CodexLike,
    ClaudeCodeLike,
}

impl BuiltinToolPresentation {
    fn surface(self, target: &ToolTarget) -> BuiltinToolSurface {
        match self {
            Self::ProviderDefault => match target.api_kind {
                ProviderApiKind::AnthropicMessages => BuiltinToolSurface::ClaudeCodeLike,
                ProviderApiKind::OpenAiResponses | ProviderApiKind::OpenAiCompletions => {
                    BuiltinToolSurface::Canonical
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
    pub run_process: bool,
    pub write_process_stdin: bool,
    pub job_start: bool,
    pub job_list: bool,
    pub job_read: bool,
    pub job_wait: bool,
    pub job_cancel: bool,
}

impl EnvironmentToolsetConfig {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn basic() -> Self {
        Self {
            run_process: true,
            write_process_stdin: true,
            ..Self::disabled()
        }
    }

    pub fn jobs() -> Self {
        Self {
            job_start: true,
            job_list: true,
            job_read: true,
            job_wait: true,
            job_cancel: true,
            ..Self::disabled()
        }
    }

    pub fn with_jobs(mut self) -> Self {
        self.job_start = true;
        self.job_list = true;
        self.job_read = true;
        self.job_wait = true;
        self.job_cancel = true;
        self
    }

    pub fn enabled(&self) -> bool {
        self.run_process
            || self.write_process_stdin
            || self.job_start
            || self.job_list
            || self.job_read
            || self.job_wait
            || self.job_cancel
    }

    fn operations(&self) -> Vec<BuiltinToolOperation> {
        let mut operations = Vec::new();
        if self.run_process {
            operations.push(BuiltinToolOperation::RunProcess);
        }
        if self.write_process_stdin {
            operations.push(BuiltinToolOperation::WriteProcessStdin);
        }
        if self.job_start {
            operations.push(BuiltinToolOperation::JobStart);
        }
        if self.job_list {
            operations.push(BuiltinToolOperation::JobList);
        }
        if self.job_read {
            operations.push(BuiltinToolOperation::JobRead);
        }
        if self.job_wait {
            operations.push(BuiltinToolOperation::JobWait);
        }
        if self.job_cancel {
            operations.push(BuiltinToolOperation::JobCancel);
        }
        operations
    }
}

pub struct ToolsetEnvironment<'a> {
    pub target: &'a ToolTarget,
}

/// Provider request parameter additions required by the resolved toolset.
///
/// The toolset only reports the required values; applying them to a session's
/// opaque provider params is owned by the runtime layer that knows the params
/// schema.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderParamsPatch {
    openai_responses_include: Vec<String>,
}

impl ProviderParamsPatch {
    pub fn is_empty(&self) -> bool {
        self.openai_responses_include.is_empty()
    }

    /// OpenAI Responses `include` values the toolset needs on generation
    /// requests.
    pub fn openai_responses_include(&self) -> &[String] {
        &self.openai_responses_include
    }

    fn add_openai_web_search(&mut self, config: &OpenAiResponsesWebSearchConfig) {
        let mut include = Vec::new();
        apply_openai_responses_web_search_includes(&mut include, config);
        for value in include {
            if !self
                .openai_responses_include
                .iter()
                .any(|existing| existing == &value)
            {
                self.openai_responses_include.push(value);
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedToolset {
    pub tools: BTreeMap<ToolName, ToolSpec>,
    pub documents: Vec<ToolDocument>,
    pub catalog: ToolCatalog,
    pub provider_params_patch: ProviderParamsPatch,
}

pub fn resolve_toolset(
    env: ToolsetEnvironment<'_>,
    config: &ToolsetConfig,
) -> ToolResult<ResolvedToolset> {
    let mut builder = ToolsetBuilder::new();

    if config.builtin.enabled() {
        builder.add_builtin_tools(env.target, &config.builtin)?;
    }

    if config.openai_web_search.enabled() {
        if env.target.api_kind != ProviderApiKind::OpenAiResponses {
            return Err(ToolError::UnsupportedCapability {
                message: format!(
                    "web.search currently supports {:?}, got {:?}",
                    ProviderApiKind::OpenAiResponses,
                    env.target.api_kind
                ),
            });
        }
        let bundle = openai_responses_web_search_tool_bundle(&config.openai_web_search)?
            .ok_or_else(|| ToolError::InvalidRequest {
                message: "web.search was enabled but did not produce a provider tool".to_owned(),
            })?;
        builder.add_provider_tool_bundle(bundle);
        builder
            .provider_params_patch
            .add_openai_web_search(&config.openai_web_search);
    }

    if config.web_fetch.enabled {
        let bundle =
            web_fetch_tool_bundle(&config.web_fetch)?.ok_or_else(|| ToolError::InvalidRequest {
                message: "web.fetch was enabled but did not produce a function tool".to_owned(),
            })?;
        builder.add_web_fetch(bundle);
    }

    if config.messaging.enabled {
        builder.add_messaging(messaging_tool_bundles(&config.messaging)?);
    }

    if config.fleet.enabled {
        builder.add_fleet(fleet_tool_bundles(&config.fleet)?);
    }

    Ok(builder.finish())
}

struct ToolsetBuilder {
    tools: BTreeMap<ToolName, ToolSpec>,
    catalog: ToolCatalog,
    documents_by_ref: BTreeMap<engine::BlobRef, ToolDocument>,
    visible_tools: Vec<ToolName>,
    seen_tools: BTreeSet<ToolName>,
    provider_params_patch: ProviderParamsPatch,
}

impl ToolsetBuilder {
    fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
            catalog: ToolCatalog::new(),
            documents_by_ref: BTreeMap::new(),
            visible_tools: Vec::new(),
            seen_tools: BTreeSet::new(),
            provider_params_patch: ProviderParamsPatch::default(),
        }
    }

    fn add_builtin_tools(
        &mut self,
        target: &ToolTarget,
        config: &BuiltinToolsetConfig,
    ) -> ToolResult<()> {
        let surface = config.presentation.surface(target);
        let omit_unsupported = config.presentation == BuiltinToolPresentation::ProviderDefault;
        for operation in config
            .fs
            .operations()
            .into_iter()
            .chain(config.process.operations())
        {
            let tool = BuiltinTool::new(operation, surface);
            let bundle = match tool.spec_bundle(target, STATIC_SCOPED_FS_PATHS) {
                Ok(bundle) => bundle,
                Err(ToolError::UnsupportedCapability { .. }) if omit_unsupported => continue,
                Err(error) => return Err(error),
            };
            let binding = tool.binding(target, config.execution.clone());
            self.add_bundle(bundle);
            self.catalog.insert(binding);
        }
        Ok(())
    }

    fn add_provider_tool_bundle(&mut self, bundle: ToolSpecBundle) {
        self.add_bundle(bundle);
    }

    fn add_web_fetch(&mut self, bundle: ToolSpecBundle) {
        self.add_bundle(bundle);
        self.catalog
            .insert(web_fetch_tool_binding(ToolExecutionMode::Inline));
    }

    fn add_messaging(&mut self, bundles: Vec<ToolSpecBundle>) {
        for bundle in bundles {
            self.add_bundle(bundle);
        }
        for binding in messaging_tool_bindings(ToolExecutionMode::Inline) {
            self.catalog.insert(binding);
        }
    }

    fn add_fleet(&mut self, bundles: Vec<ToolSpecBundle>) {
        for bundle in bundles {
            self.add_bundle(bundle);
        }
        for binding in fleet_tool_bindings(ToolExecutionMode::Inline) {
            self.catalog.insert(binding);
        }
    }

    fn add_bundle(&mut self, bundle: ToolSpecBundle) {
        let tool_name = bundle.spec.name.clone();
        if !self.seen_tools.insert(tool_name.clone()) {
            return;
        }
        for document in bundle.documents {
            self.documents_by_ref
                .entry(document.blob_ref.clone())
                .or_insert(document);
        }
        self.tools.insert(tool_name.clone(), bundle.spec);
        self.visible_tools.push(tool_name);
    }

    fn finish(self) -> ResolvedToolset {
        ResolvedToolset {
            tools: self.tools,
            documents: self.documents_by_ref.into_values().collect(),
            catalog: self.catalog,
            provider_params_patch: self.provider_params_patch,
        }
    }
}

const STATIC_SCOPED_FS_PATHS: bool = true;

#[cfg(test)]
mod tests {
    use engine::ToolTargetRequirement;
    use serde_json::{Value, json};

    use super::*;
    use crate::web::fetch::WEB_FETCH_TOOL_NAME;
    use crate::web::search::{WebSearchContextSize, WebSearchMode};

    fn target(api_kind: ProviderApiKind) -> ToolTarget {
        ToolTarget::api_kind(api_kind)
    }

    fn visible_names(toolset: &ResolvedToolset) -> Vec<String> {
        toolset
            .tools
            .keys()
            .map(|name| name.as_str().to_owned())
            .collect()
    }

    #[test]
    fn workspace_toolset_renders_openai_canonical_builtin_tools() {
        let target = target(ProviderApiKind::OpenAiResponses);

        let toolset = resolve_toolset(
            ToolsetEnvironment { target: &target },
            &ToolsetConfig::workspace(),
        )
        .expect("toolset");

        assert_eq!(
            visible_names(&toolset),
            vec![
                "apply_patch",
                "edit_file",
                "glob",
                "grep",
                "list_dir",
                "read_file",
                "write_file"
            ]
        );
        assert!(toolset.catalog.get(&ToolName::new("read_file")).is_some());
        assert!(toolset.provider_params_patch.is_empty());
    }

    #[test]
    fn builtin_tool_presentation_defaults_to_claude_style_for_anthropic() {
        let target = target(ProviderApiKind::AnthropicMessages);
        let mut config = ToolsetConfig::empty();
        config.builtin = BuiltinToolsetConfig {
            fs: FilesystemToolsetConfig {
                read_file: true,
                ..FilesystemToolsetConfig::disabled()
            },
            ..BuiltinToolsetConfig::disabled()
        };

        let toolset =
            resolve_toolset(ToolsetEnvironment { target: &target }, &config).expect("toolset");

        assert_eq!(visible_names(&toolset), vec!["Read"]);
        assert!(
            toolset
                .documents
                .iter()
                .any(|document| document.text_lossy().contains("\"file_path\""))
        );
    }

    #[test]
    fn workspace_provider_default_omits_builtin_tools_unsupported_by_provider_surface() {
        let target = target(ProviderApiKind::AnthropicMessages);

        let toolset = resolve_toolset(
            ToolsetEnvironment { target: &target },
            &ToolsetConfig::workspace(),
        )
        .expect("toolset");

        assert_eq!(
            visible_names(&toolset),
            vec!["Edit", "Glob", "Grep", "Read", "Write"]
        );
    }

    #[test]
    fn web_search_adds_provider_native_tool_and_defaults_patch() {
        let target = target(ProviderApiKind::OpenAiResponses);
        let mut config = ToolsetConfig::empty();
        config.openai_web_search = OpenAiResponsesWebSearchConfig {
            mode: WebSearchMode::Cached,
            search_context_size: Some(WebSearchContextSize::Low),
            allowed_domains: vec!["docs.rs".to_owned()],
            blocked_domains: Vec::new(),
            user_location: None,
            include_sources: true,
        };

        let toolset =
            resolve_toolset(ToolsetEnvironment { target: &target }, &config).expect("toolset");

        assert_eq!(visible_names(&toolset), vec!["web_search"]);
        assert!(toolset.catalog.is_empty());
        let native: Value =
            serde_json::from_slice(&toolset.documents[0].bytes).expect("native tool json");
        assert_eq!(
            native,
            json!({
                "type": "web_search",
                "external_web_access": false,
                "search_context_size": "low",
                "filters": { "allowed_domains": ["docs.rs"] }
            })
        );

        assert_eq!(
            toolset.provider_params_patch.openai_responses_include(),
            [crate::web::search::OPENAI_RESPONSES_WEB_SEARCH_SOURCES_INCLUDE.to_owned()]
        );
    }

    #[test]
    fn web_search_rejects_non_openai_responses_target() {
        let target = target(ProviderApiKind::AnthropicMessages);
        let mut config = ToolsetConfig::empty();
        config.openai_web_search = OpenAiResponsesWebSearchConfig::cached();

        let error = resolve_toolset(ToolsetEnvironment { target: &target }, &config)
            .expect_err("web search should reject Anthropic target");

        assert!(matches!(error, ToolError::UnsupportedCapability { .. }));
    }

    #[test]
    fn web_fetch_adds_standard_function_tool_and_catalog_binding() {
        let target = target(ProviderApiKind::OpenAiResponses);
        let mut config = ToolsetConfig::empty();
        config.web_fetch = WebFetchToolConfig::enabled();

        let toolset =
            resolve_toolset(ToolsetEnvironment { target: &target }, &config).expect("toolset");

        assert_eq!(visible_names(&toolset), vec![WEB_FETCH_TOOL_NAME]);
        assert!(
            toolset
                .catalog
                .get(&ToolName::new(WEB_FETCH_TOOL_NAME))
                .is_some()
        );
        let spec = toolset
            .tools
            .get(&ToolName::new(WEB_FETCH_TOOL_NAME))
            .expect("web_fetch spec");
        assert_eq!(spec.target_requirement, ToolTargetRequirement::None);
    }
}
