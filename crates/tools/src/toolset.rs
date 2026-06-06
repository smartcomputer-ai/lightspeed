//! Provider-aware session toolset composition.

use std::collections::{BTreeMap, BTreeSet};

use engine::{
    OpenAiResponsesRequestDefaults, ProviderApiKind, ProviderRequestDefaults, ToolChoice, ToolName,
    ToolProfile, ToolProfileId, ToolRegistry,
};

use crate::{
    error::{ToolError, ToolResult},
    host::{
        HostToolContext,
        tools::{HostTool, HostToolOperation, HostToolSurface},
    },
    runtime::{ToolCatalog, ToolDocument, ToolExecutionMode, ToolSpecBundle, ToolTarget},
    web::search::{
        OpenAiResponsesWebSearchConfig, apply_openai_responses_web_search_defaults,
        openai_responses_web_search_tool_bundle,
    },
};

pub const DEFAULT_TOOLSET_PROFILE_ID: &str = "default_tools";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolsetConfig {
    pub profile_id: ToolProfileId,
    pub host: HostToolsetConfig,
    pub openai_web_search: OpenAiResponsesWebSearchConfig,
    pub tool_choice: Option<ToolChoice>,
}

impl ToolsetConfig {
    pub fn empty() -> Self {
        Self {
            profile_id: ToolProfileId::new(DEFAULT_TOOLSET_PROFILE_ID),
            host: HostToolsetConfig::disabled(),
            openai_web_search: OpenAiResponsesWebSearchConfig::default(),
            tool_choice: None,
        }
    }

    pub fn workspace() -> Self {
        Self {
            host: HostToolsetConfig::workspace(),
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
pub struct HostToolsetConfig {
    pub presentation: HostToolPresentation,
    pub fs: HostFsToolsetConfig,
    pub process: HostProcessToolsetConfig,
    pub execution: ToolExecutionMode,
}

impl HostToolsetConfig {
    pub fn disabled() -> Self {
        Self {
            presentation: HostToolPresentation::ProviderDefault,
            fs: HostFsToolsetConfig::disabled(),
            process: HostProcessToolsetConfig::disabled(),
            execution: ToolExecutionMode::Inline,
        }
    }

    pub fn workspace() -> Self {
        Self {
            fs: HostFsToolsetConfig::workspace_edit(),
            ..Self::disabled()
        }
    }

    pub fn enabled(&self) -> bool {
        self.fs.enabled() || self.process.enabled()
    }
}

impl Default for HostToolsetConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HostToolPresentation {
    #[default]
    ProviderDefault,
    Canonical,
    CodexLike,
    ClaudeCodeLike,
}

impl HostToolPresentation {
    fn surface(self, target: &ToolTarget) -> HostToolSurface {
        match self {
            Self::ProviderDefault => match target.api_kind {
                ProviderApiKind::AnthropicMessages => HostToolSurface::ClaudeCodeLike,
                ProviderApiKind::OpenAiResponses | ProviderApiKind::OpenAiCompletions => {
                    HostToolSurface::Canonical
                }
            },
            Self::Canonical => HostToolSurface::Canonical,
            Self::CodexLike => HostToolSurface::CodexLike,
            Self::ClaudeCodeLike => HostToolSurface::ClaudeCodeLike,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostFsToolsetConfig {
    pub read_file: bool,
    pub write_file: bool,
    pub edit_file: bool,
    pub apply_patch: bool,
    pub grep: bool,
    pub glob: bool,
    pub list_dir: bool,
}

impl HostFsToolsetConfig {
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

    fn operations(&self) -> Vec<HostToolOperation> {
        let mut operations = Vec::new();
        if self.read_file {
            operations.push(HostToolOperation::ReadFile);
        }
        if self.write_file {
            operations.push(HostToolOperation::WriteFile);
        }
        if self.edit_file {
            operations.push(HostToolOperation::EditFile);
        }
        if self.apply_patch {
            operations.push(HostToolOperation::ApplyPatch);
        }
        if self.grep {
            operations.push(HostToolOperation::Grep);
        }
        if self.glob {
            operations.push(HostToolOperation::Glob);
        }
        if self.list_dir {
            operations.push(HostToolOperation::ListDir);
        }
        operations
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostProcessToolsetConfig {
    pub run_process: bool,
    pub write_process_stdin: bool,
}

impl HostProcessToolsetConfig {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn basic() -> Self {
        Self {
            run_process: true,
            write_process_stdin: true,
        }
    }

    pub fn enabled(&self) -> bool {
        self.run_process || self.write_process_stdin
    }

    fn operations(&self) -> Vec<HostToolOperation> {
        let mut operations = Vec::new();
        if self.run_process {
            operations.push(HostToolOperation::RunProcess);
        }
        if self.write_process_stdin {
            operations.push(HostToolOperation::WriteProcessStdin);
        }
        operations
    }
}

pub struct ToolsetEnvironment<'a> {
    pub target: &'a ToolTarget,
    pub host: Option<&'a HostToolContext>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderRequestDefaultsPatch {
    openai_responses_include: Vec<String>,
}

impl ProviderRequestDefaultsPatch {
    pub fn is_empty(&self) -> bool {
        self.openai_responses_include.is_empty()
    }

    pub fn apply_to(&self, defaults: &mut ProviderRequestDefaults) -> ToolResult<()> {
        if self.openai_responses_include.is_empty() {
            return Ok(());
        }

        let defaults = match defaults {
            ProviderRequestDefaults::OpenAiResponses(defaults) => defaults,
            ProviderRequestDefaults::None => {
                *defaults = ProviderRequestDefaults::OpenAiResponses(
                    OpenAiResponsesRequestDefaults::default(),
                );
                let ProviderRequestDefaults::OpenAiResponses(defaults) = defaults else {
                    unreachable!("just assigned OpenAI Responses defaults")
                };
                defaults
            }
            other => {
                return Err(ToolError::InvalidRequest {
                    message: format!(
                        "OpenAI Responses tool defaults cannot apply to request defaults {other:?}"
                    ),
                });
            }
        };

        for include in &self.openai_responses_include {
            if !defaults.include.iter().any(|existing| existing == include) {
                defaults.include.push(include.clone());
            }
        }
        Ok(())
    }

    fn add_openai_web_search(&mut self, config: &OpenAiResponsesWebSearchConfig) {
        let mut defaults = OpenAiResponsesRequestDefaults::default();
        defaults.include.clear();
        apply_openai_responses_web_search_defaults(&mut defaults, config);
        for include in defaults.include {
            if !self
                .openai_responses_include
                .iter()
                .any(|existing| existing == &include)
            {
                self.openai_responses_include.push(include);
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedToolset {
    pub profile_id: ToolProfileId,
    pub registry: ToolRegistry,
    pub documents: Vec<ToolDocument>,
    pub catalog: ToolCatalog,
    pub provider_request_defaults_patch: ProviderRequestDefaultsPatch,
}

pub fn resolve_toolset(
    env: ToolsetEnvironment<'_>,
    config: &ToolsetConfig,
) -> ToolResult<ResolvedToolset> {
    let mut builder = ToolsetBuilder::new(config.profile_id.clone(), config.tool_choice.clone());

    if config.host.enabled() {
        let host = env.host.ok_or_else(|| ToolError::InvalidRequest {
            message: "host tools require a host tool context".to_owned(),
        })?;
        builder.add_host_tools(host, env.target, &config.host)?;
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
            .provider_request_defaults_patch
            .add_openai_web_search(&config.openai_web_search);
    }

    Ok(builder.finish())
}

struct ToolsetBuilder {
    profile_id: ToolProfileId,
    tool_choice: Option<ToolChoice>,
    registry: ToolRegistry,
    catalog: ToolCatalog,
    documents_by_ref: BTreeMap<engine::BlobRef, ToolDocument>,
    visible_tools: Vec<ToolName>,
    seen_tools: BTreeSet<ToolName>,
    provider_request_defaults_patch: ProviderRequestDefaultsPatch,
}

impl ToolsetBuilder {
    fn new(profile_id: ToolProfileId, tool_choice: Option<ToolChoice>) -> Self {
        Self {
            profile_id,
            tool_choice,
            registry: ToolRegistry::default(),
            catalog: ToolCatalog::new(),
            documents_by_ref: BTreeMap::new(),
            visible_tools: Vec::new(),
            seen_tools: BTreeSet::new(),
            provider_request_defaults_patch: ProviderRequestDefaultsPatch::default(),
        }
    }

    fn add_host_tools(
        &mut self,
        host: &HostToolContext,
        target: &ToolTarget,
        config: &HostToolsetConfig,
    ) -> ToolResult<()> {
        let scoped_paths = is_scoped_host(host);
        let surface = config.presentation.surface(target);
        let omit_unsupported = config.presentation == HostToolPresentation::ProviderDefault;
        for operation in config
            .fs
            .operations()
            .into_iter()
            .chain(config.process.operations())
        {
            let tool = HostTool::new(operation, surface);
            let bundle = match tool.spec_bundle(target, scoped_paths) {
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
        self.registry.tools.insert(tool_name.clone(), bundle.spec);
        self.visible_tools.push(tool_name);
    }

    fn finish(mut self) -> ResolvedToolset {
        self.registry.profiles.insert(
            self.profile_id.clone(),
            ToolProfile {
                profile_id: self.profile_id.clone(),
                visible_tools: self.visible_tools,
                tool_choice: self.tool_choice,
            },
        );
        ResolvedToolset {
            profile_id: self.profile_id,
            registry: self.registry,
            documents: self.documents_by_ref.into_values().collect(),
            catalog: self.catalog,
            provider_request_defaults_patch: self.provider_request_defaults_patch,
        }
    }
}

fn is_scoped_host(host: &HostToolContext) -> bool {
    matches!(
        host.fs.access_policy(),
        crate::host::fs::FileAccessPolicy::ScopedReadWrite { .. }
            | crate::host::fs::FileAccessPolicy::ScopedReadOnly { .. }
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::{ProviderRequestDefaults, storage::InMemoryBlobStore};
    use serde_json::{Value, json};

    use super::*;
    use crate::host::fs::{FileAccessPolicy, InMemoryFileSystem};
    use crate::web::search::{WebSearchContextSize, WebSearchMode};

    fn host(policy: FileAccessPolicy) -> HostToolContext {
        HostToolContext::new(
            Arc::new(InMemoryFileSystem::new(policy)),
            None,
            Arc::new(InMemoryBlobStore::new()),
        )
    }

    fn target(api_kind: ProviderApiKind) -> ToolTarget {
        ToolTarget::api_kind(api_kind)
    }

    fn visible_names(toolset: &ResolvedToolset) -> Vec<String> {
        toolset
            .registry
            .profiles
            .get(&toolset.profile_id)
            .expect("profile")
            .visible_tools
            .iter()
            .map(|name| name.as_str().to_owned())
            .collect()
    }

    #[test]
    fn workspace_toolset_renders_openai_canonical_host_tools() {
        let ctx = host(FileAccessPolicy::FullReadWrite);
        let target = target(ProviderApiKind::OpenAiResponses);

        let toolset = resolve_toolset(
            ToolsetEnvironment {
                target: &target,
                host: Some(&ctx),
            },
            &ToolsetConfig::workspace(),
        )
        .expect("toolset");

        assert_eq!(
            visible_names(&toolset),
            vec![
                "read_file",
                "write_file",
                "edit_file",
                "apply_patch",
                "grep",
                "glob",
                "list_dir"
            ]
        );
        assert!(toolset.catalog.get(&ToolName::new("read_file")).is_some());
        assert!(toolset.provider_request_defaults_patch.is_empty());
    }

    #[test]
    fn host_tool_presentation_defaults_to_claude_style_for_anthropic() {
        let ctx = host(FileAccessPolicy::FullReadOnly);
        let target = target(ProviderApiKind::AnthropicMessages);
        let mut config = ToolsetConfig::empty();
        config.host = HostToolsetConfig {
            fs: HostFsToolsetConfig {
                read_file: true,
                ..HostFsToolsetConfig::disabled()
            },
            ..HostToolsetConfig::disabled()
        };

        let toolset = resolve_toolset(
            ToolsetEnvironment {
                target: &target,
                host: Some(&ctx),
            },
            &config,
        )
        .expect("toolset");

        assert_eq!(visible_names(&toolset), vec!["Read"]);
        assert!(
            toolset
                .documents
                .iter()
                .any(|document| document.text_lossy().contains("\"file_path\""))
        );
    }

    #[test]
    fn workspace_provider_default_omits_host_tools_unsupported_by_provider_surface() {
        let ctx = host(FileAccessPolicy::FullReadWrite);
        let target = target(ProviderApiKind::AnthropicMessages);

        let toolset = resolve_toolset(
            ToolsetEnvironment {
                target: &target,
                host: Some(&ctx),
            },
            &ToolsetConfig::workspace(),
        )
        .expect("toolset");

        assert_eq!(
            visible_names(&toolset),
            vec!["Read", "Write", "Edit", "Grep", "Glob"]
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

        let toolset = resolve_toolset(
            ToolsetEnvironment {
                target: &target,
                host: None,
            },
            &config,
        )
        .expect("toolset");

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

        let mut defaults =
            ProviderRequestDefaults::OpenAiResponses(OpenAiResponsesRequestDefaults::default());
        toolset
            .provider_request_defaults_patch
            .apply_to(&mut defaults)
            .expect("apply defaults");
        let ProviderRequestDefaults::OpenAiResponses(defaults) = defaults else {
            panic!("expected OpenAI Responses defaults")
        };
        assert!(
            defaults
                .include
                .iter()
                .any(|include| { include == engine::OPENAI_RESPONSES_WEB_SEARCH_SOURCES_INCLUDE })
        );
    }

    #[test]
    fn web_search_rejects_non_openai_responses_target() {
        let target = target(ProviderApiKind::AnthropicMessages);
        let mut config = ToolsetConfig::empty();
        config.openai_web_search = OpenAiResponsesWebSearchConfig::cached();

        let error = resolve_toolset(
            ToolsetEnvironment {
                target: &target,
                host: None,
            },
            &config,
        )
        .expect_err("web search should reject Anthropic target");

        assert!(matches!(error, ToolError::UnsupportedCapability { .. }));
    }
}
