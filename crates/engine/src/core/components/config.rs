use serde::{Deserialize, Serialize};

use crate::{
    CoreAgentState, DomainError, ModelSelection, ProviderApiKind, ProviderParams, ToolChoice,
};

const MIN_OPENAI_RESPONSES_COMPACT_THRESHOLD: u32 = 1000;

/// Current version of every feature block. Bumps per feature once a breaking
/// behavior revision ships; `validate_feature_version` then becomes a
/// per-feature match over the supported set.
pub const CURRENT_FEATURE_VERSION: u32 = 1;

/// Declared session configuration.
///
/// The document is sparse: everything except `model` is optional, and an
/// omitted section means "defaults". Features follow capability semantics —
/// an absent feature is not granted (no tools, no access); a present feature
/// is granted with the defaults documented on its struct. The stored document
/// is this declaration itself; effective behavior is materialized outside the
/// engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub model: ModelSelection,
    #[serde(default, skip_serializing_if = "GenerationConfig::is_default")]
    pub generation: GenerationConfig,
    #[serde(default, skip_serializing_if = "LimitsConfig::is_default")]
    pub limits: LimitsConfig,
    #[serde(default, skip_serializing_if = "ContextConfig::is_default")]
    pub context: ContextConfig,
    #[serde(default, skip_serializing_if = "FeaturesConfig::is_default")]
    pub features: FeaturesConfig,
}

impl SessionConfig {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_generation(&self.generation, &self.model)?;
        validate_context_config(&self.context, &self.model.api_kind)?;
        validate_features(&self.features, &self.model.api_kind)
    }
}

pub(crate) fn validate_config_update_for_state(
    state: &CoreAgentState,
    config: &SessionConfig,
) -> Result<(), DomainError> {
    let current = current_config(state)?;
    validate_session_is_idle_for_config_update(state)?;
    config.validate()?;
    validate_session_api_kind_is_pinned(&current.model.api_kind, &config.model.api_kind)?;
    validate_active_context_api_kind(state, &config.model.api_kind)?;
    validate_tool_choice_for_active_tools(state, config.generation.tool_choice.as_ref())?;
    Ok(())
}

/// Turn-shaping defaults applied to every LLM generation. Per-run overrides
/// ride [`RunConfig`] on run requests.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Reasoning effort tier as a provider-native string (e.g. "none",
    /// "high", "xhigh", "max"). The engine carries it opaquely; the LLM
    /// runtime validates it against the provider and materializes the
    /// request params. Never stored as provider JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Whether the model may call several tools in one turn. `None` leaves
    /// the provider default; materialized provider-natively by the LLM
    /// runtime (OpenAI `parallel_tool_calls`, Anthropic
    /// `disable_parallel_tool_use`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_use: Option<bool>,
    /// Neutral processing class inherited by every generation in the session.
    /// Provider adapters lower it into their native request vocabulary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing_tier: Option<ModelProcessingTier>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelProcessingTier {
    Standard,
    Fast,
    Flex,
}

impl GenerationConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// Run budget defaults. Per-run overrides ride [`RunConfig`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<u32>,
}

impl LimitsConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionPolicy>,
}

impl ContextConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

// ---------------------------------------------------------------------------
// Features
//
// Capability grants: an absent feature is not granted; `{}` grants it with
// defaults. Every block carries a behavior `version`. Omitted input decodes
// to the current default and the field always serializes, so stored documents
// pin the version they were admitted with even when the default later moves.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeaturesConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfs: Option<VfsFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<SubagentsFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timers: Option<TimersFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<EnvironmentsFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpFeature>,
}

impl FeaturesConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// Grants the session virtual filesystem. Workspace attachments declare the
/// session-visible namespace and the VFS catalog is surfaced to the session.
/// The agent tool surface is derived from the attachments: any attachment
/// installs the read tools and any `edit` attachment adds the write tools.
/// Prompt/skill sourcing is granted independently.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    /// Absolute VFS tool working directory; absent uses /.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Catalog resources exposed in the session's workspace namespace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<WorkspaceAttachment>,
    /// Prompt-instruction sourcing from the VFS; absent = prompts are not
    /// sourced from the VFS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<VfsPromptsConfig>,
    /// Skill discovery sourcing from the VFS; absent = skills are not
    /// sourced from the VFS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<VfsSkillsConfig>,
}

impl Default for VfsFeature {
    fn default() -> Self {
        Self {
            version: CURRENT_FEATURE_VERSION,
            workspaces: Vec::new(),
            working_directory: None,
            prompts: None,
            skills: None,
        }
    }
}

impl VfsFeature {
    /// The widest access any workspace attachment grants; `None` without
    /// attachments, which installs no filesystem tools.
    pub fn tool_access(&self) -> Option<WorkspaceAccess> {
        self.workspaces.iter().map(|link| link.access).max()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAttachment {
    pub path: String,
    pub target: WorkspaceAttachmentTarget,
    pub access: WorkspaceAccess,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum WorkspaceAttachmentTarget {
    Workspace { workspace_id: String },
    Snapshot { snapshot_ref: String },
}

/// Per-attachment VFS access. Ordered: `edit` implies `read`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAccess {
    Read,
    Edit,
}

impl WorkspaceAccess {
    pub fn allows_edit(self) -> bool {
        self == Self::Edit
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsPromptsConfig {
    /// Absent searches .agents/prompts and .lightspeed/prompts beneath each
    /// workspace attachment. Explicit roots replace these defaults and must be
    /// non-empty absolute paths contained in workspace attachments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsSkillsConfig {
    /// Absent searches .agents/skills and .lightspeed/skills beneath each
    /// workspace attachment. Explicit roots replace these defaults and must be
    /// non-empty absolute paths contained in workspace attachments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<String>>,
}

/// Grants network access through the web toolset. `fetch` and `search` are
/// independently granted sub-capabilities; a web block granting neither is
/// rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch: Option<WebFetchFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<WebSearchFeature>,
}

impl Default for WebFeature {
    fn default() -> Self {
        Self {
            version: CURRENT_FEATURE_VERSION,
            fetch: None,
            search: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebFetchFeature {}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchFeature {
    /// `None` means all domains are searchable; an explicit list must be
    /// non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_domains: Vec<String>,
}

/// Grants sub-agent delegation: `agent_run` (joined) and `agent_spawn`
/// (promise) over the allowlisted agent profiles, bounded by root-scoped,
/// attenuating limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentsFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    /// The agent menu: profiles the model may run. Ids must name existing
    /// profiles at admission; the list is the authority.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<SubagentAgentConfig>,
    #[serde(flatten)]
    pub limits: SubagentLimits,
}

impl Default for SubagentsFeature {
    fn default() -> Self {
        Self {
            version: CURRENT_FEATURE_VERSION,
            agents: Vec::new(),
            limits: SubagentLimits::default(),
        }
    }
}

impl SubagentsFeature {
    pub fn agent_allowed(&self, profile_id: &str) -> bool {
        self.agents
            .iter()
            .any(|agent| agent.profile_id == profile_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentAgentConfig {
    pub profile_id: String,
}

/// Root-scoped sub-agent limits. Every descendant of a root session counts
/// against the root; a nested grant attenuates (element-wise minimum with
/// the limits pinned on its origin) and never widens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentLimits {
    /// A child at depth `d` may spawn only while `d + 1 <= max_depth`.
    #[serde(default = "default_subagent_max_depth")]
    pub max_depth: u32,
    /// Lifetime total of sessions ever created under the root.
    #[serde(default = "default_subagent_max_descendants")]
    pub max_descendants: u32,
    /// Open sessions under the root, excluding the root itself.
    #[serde(default = "default_subagent_max_concurrent")]
    pub max_concurrent: u32,
    /// Per-child run deadline; bounded by the execution binding's ceiling.
    #[serde(default = "default_subagent_deadline_ms")]
    pub deadline_ms: u64,
}

pub const SUBAGENT_DEFAULT_MAX_DEPTH: u32 = 2;
pub const SUBAGENT_DEFAULT_MAX_DESCENDANTS: u32 = 16;
pub const SUBAGENT_DEFAULT_MAX_CONCURRENT: u32 = 4;
pub const SUBAGENT_DEFAULT_DEADLINE_MS: u64 = 60 * 60 * 1_000;
/// Hard bound the execution binding enforces; a grant's `deadline_ms` may
/// not exceed it.
pub const SUBAGENT_DEADLINE_CEILING_MS: u64 = 24 * 60 * 60 * 1_000;

impl Default for SubagentLimits {
    fn default() -> Self {
        Self {
            max_depth: SUBAGENT_DEFAULT_MAX_DEPTH,
            max_descendants: SUBAGENT_DEFAULT_MAX_DESCENDANTS,
            max_concurrent: SUBAGENT_DEFAULT_MAX_CONCURRENT,
            deadline_ms: SUBAGENT_DEFAULT_DEADLINE_MS,
        }
    }
}

impl SubagentLimits {
    /// Element-wise minimum: the effective limits of a nested grant.
    pub fn attenuated_by(self, outer: SubagentLimits) -> Self {
        Self {
            max_depth: self.max_depth.min(outer.max_depth),
            max_descendants: self.max_descendants.min(outer.max_descendants),
            max_concurrent: self.max_concurrent.min(outer.max_concurrent),
            deadline_ms: self.deadline_ms.min(outer.deadline_ms),
        }
    }
}

fn default_subagent_max_depth() -> u32 {
    SUBAGENT_DEFAULT_MAX_DEPTH
}

fn default_subagent_max_descendants() -> u32 {
    SUBAGENT_DEFAULT_MAX_DESCENDANTS
}

fn default_subagent_max_concurrent() -> u32 {
    SUBAGENT_DEFAULT_MAX_CONCURRENT
}

fn default_subagent_deadline_ms() -> u64 {
    SUBAGENT_DEFAULT_DEADLINE_MS
}

/// Grants timer promises through the sleep tool plus the base concurrency
/// tools (await/cancel/detach).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimersFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
}

impl Default for TimersFeature {
    fn default() -> Self {
        Self {
            version: CURRENT_FEATURE_VERSION,
        }
    }
}

/// Grants session environments. The attachment list is the allowed set:
/// the session can only select, read, or run jobs on a listed machine, and
/// each attachment carries its own access grant and working directory. The
/// installed tool surface is the union of every attachment's grant; a call
/// the active machine's grant does not cover is rejected at execution, so
/// switching machines never changes the toolset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentsFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    /// Installs model-facing list/activate/deactivate tools. Environment read
    /// is present whenever the environments feature is granted.
    #[serde(default)]
    pub selection: bool,
    /// Independent environment prompt loading; absent disables sourced instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<EnvironmentPromptsConfig>,
    /// Independent environment skill discovery. Absent disables discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<EnvironmentSkillsConfig>,
    /// The environments this session may use, each with its own grant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environments: Vec<EnvironmentAttachment>,
}

impl Default for EnvironmentsFeature {
    fn default() -> Self {
        Self {
            version: CURRENT_FEATURE_VERSION,
            selection: false,
            prompts: None,
            skills: None,
            environments: Vec::new(),
        }
    }
}

impl EnvironmentsFeature {
    pub fn attachment(&self, environment_id: &str) -> Option<&EnvironmentAttachment> {
        self.environments
            .iter()
            .find(|attachment| attachment.environment_id == environment_id)
    }

    pub fn is_attached(&self, environment_id: &str) -> bool {
        self.attachment(environment_id).is_some()
    }

    /// The attachment activated when a profile is applied and nothing is
    /// active; validation admits at most one.
    pub fn default_attachment(&self) -> Option<&EnvironmentAttachment> {
        self.environments
            .iter()
            .find(|attachment| attachment.default)
    }

    /// The widest grant across attachments; the installed tool surface.
    pub fn tool_access(&self) -> Option<EnvironmentAccess> {
        self.environments
            .iter()
            .map(|attachment| attachment.access)
            .max()
    }
}

/// One environment the session may use. Access and working directory are
/// properties of the pairing, not of the machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentAttachment {
    pub environment_id: String,
    /// Activated when a profile is applied while nothing is active; never
    /// overrides a live selection.
    #[serde(default)]
    pub default: bool,
    pub access: EnvironmentAccess,
    /// Absolute machine working directory for file tools, commands, jobs,
    /// and sources; absent uses the machine's advertised default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
}

/// Per-attachment environment access. Ordered: each level implies the ones
/// before it. `exec` grants processes, which can write files regardless of
/// file-tool level, so a read-only file surface with commands is not a
/// meaningful restriction and is not expressible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAccess {
    Read,
    Edit,
    Exec,
    Jobs,
}

impl EnvironmentAccess {
    pub fn allows_edit(self) -> bool {
        self >= Self::Edit
    }

    pub fn allows_exec(self) -> bool {
        self >= Self::Exec
    }

    pub fn allows_jobs(self) -> bool {
        self >= Self::Jobs
    }

    /// The ladder as the model sees it, e.g. `read, edit, exec`.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Edit => "read, edit",
            Self::Exec => "read, edit, exec",
            Self::Jobs => "read, edit, exec, jobs",
        }
    }
}

/// Prompt loading scope resolved on the selected machine, never on the worker.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentPromptsConfig {
    /// Optional source directories, absolute or relative to the environment
    /// working directory. Explicit nonempty lists replace all defaults,
    /// including home roots. Defaults are .agents/prompts and
    /// .lightspeed/prompts under working directory and execution home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<String>>,
}

/// Skill discovery scope resolved on the selected machine, never on the worker.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentSkillsConfig {
    /// Optional source directories, absolute or relative to the environment
    /// working directory. Explicit nonempty lists replace all defaults,
    /// including home roots. Defaults are .agents/skills and
    /// .lightspeed/skills under working directory and execution home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<String>>,
}

/// Grants remote MCP tools by declaring linked servers from the universe MCP
/// catalog. Reconciliation into tool specs happens in the runtime
/// materialization layer, not in the engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    /// Attached servers have unique ids. An empty list grants no MCP tools.
    #[serde(default)]
    pub servers: Vec<McpServerLink>,
}

impl Default for McpFeature {
    fn default() -> Self {
        Self {
            version: CURRENT_FEATURE_VERSION,
            servers: Vec::new(),
        }
    }
}

/// A selected universe MCP server. Its catalog record owns connection,
/// execution, exposure, approval, and auth; the link may only narrow the
/// record's tool allowlist for this session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerLink {
    pub server_id: String,
    /// Subset of the record's allowed tools exposed to this session; absent
    /// exposes the record's full allowlist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}

fn default_feature_version() -> u32 {
    CURRENT_FEATURE_VERSION
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum CompactionPolicy {
    Disabled,
    ProviderTriggered {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compact_threshold_tokens: Option<u32>,
    },
    ProviderStandalone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compact_threshold_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_tokens: Option<u32>,
    },
}

// ---------------------------------------------------------------------------
// Per-run overrides
// ---------------------------------------------------------------------------

/// Per-run overrides carried on run requests. Not part of [`SessionConfig`]:
/// session-level defaults live in [`GenerationConfig`] and [`LimitsConfig`];
/// this is the runs/start escape hatch, including raw provider params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing_tier: Option<ModelProcessingTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_params: Option<ProviderParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_use: Option<bool>,
}

impl RunConfig {
    pub fn validate_provider_compatibility(
        &self,
        session_api_kind: &ProviderApiKind,
    ) -> Result<(), DomainError> {
        let api_kind = if let Some(model) = self.model_override.as_ref() {
            if &model.api_kind != session_api_kind {
                return Err(DomainError::ProviderCompatibility(format!(
                    "run model override api kind {:?} does not match session api kind {:?}",
                    model.api_kind, session_api_kind
                )));
            }
            &model.api_kind
        } else {
            session_api_kind
        };
        if self.processing_tier.is_some()
            && !matches!(
                api_kind,
                ProviderApiKind::OpenAiResponses | ProviderApiKind::OpenAiCompletions
            )
        {
            return Err(DomainError::ProviderCompatibility(
                "processing tier requires an OpenAI API kind".to_owned(),
            ));
        }
        if self.processing_tier.is_some()
            && self
                .model_override
                .as_ref()
                .is_some_and(|model| model.provider_id != "openai")
        {
            return Err(DomainError::ProviderCompatibility(
                "processing tier is supported only by the built-in openai provider".to_owned(),
            ));
        }
        validate_provider_params(self.provider_params.as_ref(), api_kind)?;
        Ok(())
    }
}

pub(crate) fn validate_run_config_for_state(
    state: &CoreAgentState,
    run_config: &RunConfig,
) -> Result<(), DomainError> {
    let config = current_config(state)?;
    run_config.validate_provider_compatibility(&config.model.api_kind)?;
    validate_active_context_api_kind(state, &config.model.api_kind)?;
    validate_tool_choice_for_active_tools(state, run_config.tool_choice.as_ref())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate_generation(
    generation: &GenerationConfig,
    model: &ModelSelection,
) -> Result<(), DomainError> {
    if generation
        .reasoning_effort
        .as_ref()
        .is_some_and(|effort| effort.trim().is_empty())
    {
        return Err(DomainError::InvariantViolation(
            "reasoning_effort must be a non-empty string when set".to_owned(),
        ));
    }
    if generation.processing_tier.is_some()
        && (model.provider_id != "openai"
            || !matches!(
                model.api_kind,
                ProviderApiKind::OpenAiResponses | ProviderApiKind::OpenAiCompletions
            ))
    {
        return Err(DomainError::ProviderCompatibility(
            "processing tier is supported only by the built-in openai provider".to_owned(),
        ));
    }
    Ok(())
}

fn validate_features(
    features: &FeaturesConfig,
    api_kind: &ProviderApiKind,
) -> Result<(), DomainError> {
    if let Some(vfs) = &features.vfs {
        validate_feature_version("vfs", vfs.version)?;
        let link_paths = validate_workspace_attachments(&vfs.workspaces)?;
        if let Some(cwd) = &vfs.working_directory
            && cwd != "/"
        {
            validate_source_roots(
                "vfs working directory",
                Some(std::slice::from_ref(cwd)),
                &link_paths,
            )?;
        }
        if let Some(prompts) = &vfs.prompts {
            validate_source_roots("vfs prompts", prompts.roots.as_deref(), &link_paths)?;
        }
        if let Some(skills) = &vfs.skills {
            validate_source_roots("vfs skills", skills.roots.as_deref(), &link_paths)?;
        }
    }
    if let Some(web) = &features.web {
        validate_feature_version("web", web.version)?;
        validate_web_feature(web, api_kind)?;
    }
    if let Some(subagents) = &features.subagents {
        validate_feature_version("subagents", subagents.version)?;
        validate_subagents_feature(subagents)?;
    }
    if let Some(timers) = &features.timers {
        validate_feature_version("timers", timers.version)?;
    }
    if let Some(environments) = &features.environments {
        validate_feature_version("environments", environments.version)?;
        validate_environment_attachments(&environments.environments)?;
        for roots in [
            environments
                .skills
                .as_ref()
                .and_then(|source| source.roots.as_deref()),
            environments
                .prompts
                .as_ref()
                .and_then(|source| source.roots.as_deref()),
        ]
        .into_iter()
        .flatten()
        {
            if roots.is_empty()
                || roots
                    .iter()
                    .any(|root| root.trim().is_empty() || root.contains('\0'))
            {
                return Err(DomainError::InvariantViolation(
                    "environment source root overrides must be nonempty paths".into(),
                ));
            }
        }
    }
    if let Some(mcp) = &features.mcp {
        validate_feature_version("mcp", mcp.version)?;
        validate_mcp_feature(mcp)?;
    }
    Ok(())
}

fn validate_subagents_feature(subagents: &SubagentsFeature) -> Result<(), DomainError> {
    if subagents.agents.is_empty() {
        return Err(DomainError::InvariantViolation(
            "subagents feature must list at least one agent profile".to_owned(),
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for agent in &subagents.agents {
        if agent.profile_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "subagents agent profile id must be non-empty".to_owned(),
            ));
        }
        if !seen.insert(agent.profile_id.as_str()) {
            return Err(DomainError::InvariantViolation(format!(
                "subagents agent profile id is listed twice: {}",
                agent.profile_id
            )));
        }
    }
    let limits = &subagents.limits;
    if limits.max_depth == 0 || limits.max_descendants == 0 || limits.max_concurrent == 0 {
        return Err(DomainError::InvariantViolation(
            "subagents limits maxDepth, maxDescendants, and maxConcurrent must be at least 1"
                .to_owned(),
        ));
    }
    if limits.deadline_ms == 0 || limits.deadline_ms > SUBAGENT_DEADLINE_CEILING_MS {
        return Err(DomainError::InvariantViolation(format!(
            "subagents deadlineMs must be between 1 and {SUBAGENT_DEADLINE_CEILING_MS}"
        )));
    }
    Ok(())
}

fn validate_environment_attachments(
    attachments: &[EnvironmentAttachment],
) -> Result<(), DomainError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut defaults = 0;
    for attachment in attachments {
        crate::EnvironmentId::try_new(attachment.environment_id.clone()).map_err(|error| {
            DomainError::InvariantViolation(format!("invalid environment attachment: {error}"))
        })?;
        if !seen.insert(attachment.environment_id.as_str()) {
            return Err(DomainError::InvariantViolation(format!(
                "environment {} is attached more than once",
                attachment.environment_id
            )));
        }
        if attachment.default {
            defaults += 1;
        }
        if let Some(cwd) = &attachment.working_directory
            && (!cwd.starts_with('/') || cwd.contains('\0'))
        {
            return Err(DomainError::InvariantViolation(format!(
                "environment {} working directory must be absolute",
                attachment.environment_id
            )));
        }
    }
    if defaults > 1 {
        return Err(DomainError::InvariantViolation(
            "at most one environment attachment may be the default".to_owned(),
        ));
    }
    Ok(())
}

fn validate_workspace_attachments(
    links: &[WorkspaceAttachment],
) -> Result<Vec<String>, DomainError> {
    let mut paths = Vec::with_capacity(links.len());
    for link in links {
        let path = canonical_workspace_attachment_path(&link.path)?;
        if paths.iter().any(|existing: &String| {
            existing == &path
                || existing == "/"
                || path == "/"
                || existing.starts_with(&format!("{path}/"))
                || path.starts_with(&format!("{existing}/"))
        }) {
            return Err(DomainError::InvariantViolation(format!(
                "workspace attachment path {path:?} overlaps another workspace attachment"
            )));
        }
        match &link.target {
            WorkspaceAttachmentTarget::Workspace { workspace_id } => {
                if workspace_id.trim().is_empty() {
                    return Err(DomainError::InvariantViolation(
                        "workspace attachment workspace_id must not be empty".to_owned(),
                    ));
                }
            }
            WorkspaceAttachmentTarget::Snapshot { snapshot_ref } => {
                if snapshot_ref.trim().is_empty() {
                    return Err(DomainError::InvariantViolation(
                        "workspace attachment snapshot_ref must not be empty".to_owned(),
                    ));
                }
                if link.access != WorkspaceAccess::Read {
                    return Err(DomainError::InvariantViolation(format!(
                        "snapshot workspace attachment at {path:?} must be read"
                    )));
                }
            }
        }
        paths.push(path);
    }
    Ok(paths)
}

fn canonical_workspace_attachment_path(path: &str) -> Result<String, DomainError> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(DomainError::InvariantViolation(format!(
            "workspace attachment path {path:?} must be absolute"
        )));
    }
    if path.len() > 1 && path.ends_with('/') {
        return Err(DomainError::InvariantViolation(format!(
            "workspace attachment path {path:?} must be canonical"
        )));
    }
    if path
        .split('/')
        .skip(1)
        .any(|part| part.is_empty() || part == "." || part == ".." || part.contains('\0'))
    {
        return Err(DomainError::InvariantViolation(format!(
            "workspace attachment path {path:?} must be canonical"
        )));
    }
    Ok(path.to_owned())
}

fn validate_feature_version(feature: &str, version: u32) -> Result<(), DomainError> {
    if version == CURRENT_FEATURE_VERSION {
        Ok(())
    } else {
        Err(DomainError::InvariantViolation(format!(
            "unsupported {} feature version {}; supported: {}",
            feature, version, CURRENT_FEATURE_VERSION
        )))
    }
}

fn validate_source_roots(
    feature: &str,
    roots: Option<&[String]>,
    link_paths: &[String],
) -> Result<(), DomainError> {
    let Some(roots) = roots else {
        return Ok(());
    };
    if roots.is_empty() {
        return Err(DomainError::InvariantViolation(format!(
            "explicit {} roots must be non-empty; omit the sourcing block to disable it",
            feature
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    for root in roots {
        let root = canonical_workspace_attachment_path(root)?;
        if !seen.insert(root.clone()) {
            return Err(DomainError::InvariantViolation(format!(
                "{feature} root {root:?} is declared more than once"
            )));
        }
        if !link_paths.iter().any(|link_path| {
            root == *link_path || link_path == "/" || root.starts_with(&format!("{link_path}/"))
        }) {
            return Err(DomainError::InvariantViolation(format!(
                "{feature} root {root:?} is not under a workspace attachment"
            )));
        }
    }
    Ok(())
}

fn validate_web_feature(web: &WebFeature, api_kind: &ProviderApiKind) -> Result<(), DomainError> {
    if web.fetch.is_none() && web.search.is_none() {
        return Err(DomainError::InvariantViolation(
            "web feature grants neither fetch nor search; omit the feature instead".to_owned(),
        ));
    }
    if let Some(search) = &web.search {
        if !matches!(
            api_kind,
            ProviderApiKind::OpenAiResponses | ProviderApiKind::AnthropicMessages
        ) {
            return Err(DomainError::ProviderCompatibility(format!(
                "web search requires OpenAI Responses or Anthropic Messages api kind, got {:?}",
                api_kind
            )));
        }
        if let Some(allowed) = &search.allowed_domains {
            if allowed.is_empty() {
                return Err(DomainError::InvariantViolation(
                    "explicit web search allowed_domains must be non-empty; omit for all domains"
                        .to_owned(),
                ));
            }
            if allowed.iter().any(|domain| domain.trim().is_empty()) {
                return Err(DomainError::InvariantViolation(
                    "web search allowed_domains must not contain empty entries".to_owned(),
                ));
            }
        }
        if search
            .blocked_domains
            .iter()
            .any(|domain| domain.trim().is_empty())
        {
            return Err(DomainError::InvariantViolation(
                "web search blocked_domains must not contain empty entries".to_owned(),
            ));
        }
        if api_kind == &ProviderApiKind::AnthropicMessages
            && search.allowed_domains.is_some()
            && !search.blocked_domains.is_empty()
        {
            return Err(DomainError::ProviderCompatibility(
                "Anthropic web search accepts allowedDomains or blockedDomains, not both"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_mcp_feature(mcp: &McpFeature) -> Result<(), DomainError> {
    let mut seen = std::collections::BTreeSet::new();
    for link in &mcp.servers {
        if link.server_id.trim().is_empty() {
            return Err(DomainError::InvariantViolation(
                "mcp server link requires a non-empty server_id".to_owned(),
            ));
        }
        if !seen.insert(link.server_id.as_str()) {
            return Err(DomainError::InvariantViolation(format!(
                "mcp server {} is linked more than once",
                link.server_id
            )));
        }
        if let Some(tools) = &link.tools {
            if tools.is_empty() {
                return Err(DomainError::InvariantViolation(format!(
                    "mcp server {} tools subset must be non-empty; omit it to expose the record's allowlist",
                    link.server_id
                )));
            }
            let mut names = std::collections::BTreeSet::new();
            for tool in tools {
                if tool.trim().is_empty() {
                    return Err(DomainError::InvariantViolation(format!(
                        "mcp server {} tools subset contains an empty tool name",
                        link.server_id
                    )));
                }
                if !names.insert(tool.as_str()) {
                    return Err(DomainError::InvariantViolation(format!(
                        "mcp server {} tools subset lists {tool} twice",
                        link.server_id
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_tool_choice_for_active_tools(
    state: &CoreAgentState,
    tool_choice: Option<&ToolChoice>,
) -> Result<(), DomainError> {
    let Some(ToolChoice::Specific { tool_name }) = tool_choice else {
        return Ok(());
    };
    if state.tooling.tools.contains_key(tool_name) {
        Ok(())
    } else {
        Err(DomainError::InvariantViolation(format!(
            "tool_choice references missing active tool {}",
            tool_name
        )))
    }
}

fn validate_provider_params(
    params: Option<&ProviderParams>,
    api_kind: &ProviderApiKind,
) -> Result<(), DomainError> {
    let Some(params) = params else {
        return Ok(());
    };
    if &params.api_kind != api_kind {
        return Err(DomainError::ProviderCompatibility(format!(
            "provider params api kind {:?} do not match provider api kind {:?}",
            params.api_kind, api_kind
        )));
    }
    if !params.body.is_object() {
        return Err(DomainError::ProviderCompatibility(
            "provider params body must be a JSON object".to_owned(),
        ));
    }
    Ok(())
}

fn validate_context_config(
    context: &ContextConfig,
    api_kind: &ProviderApiKind,
) -> Result<(), DomainError> {
    match (&context.compaction, api_kind) {
        (None | Some(CompactionPolicy::Disabled), _) => Ok(()),
        (
            Some(CompactionPolicy::ProviderTriggered {
                compact_threshold_tokens,
            }),
            ProviderApiKind::OpenAiResponses,
        ) => validate_openai_responses_compact_threshold(*compact_threshold_tokens),
        (
            Some(CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens,
                target_tokens,
            }),
            ProviderApiKind::OpenAiResponses
            | ProviderApiKind::OpenAiCompletions
            | ProviderApiKind::AnthropicMessages,
        ) => validate_provider_standalone_compaction(*compact_threshold_tokens, *target_tokens),
        (Some(CompactionPolicy::ProviderTriggered { .. }), api_kind) => {
            Err(DomainError::ProviderCompatibility(format!(
                "provider-triggered compaction requires OpenAI Responses api kind, got {:?}",
                api_kind
            )))
        }
    }
}

fn validate_openai_responses_compact_threshold(
    compact_threshold_tokens: Option<u32>,
) -> Result<(), DomainError> {
    if compact_threshold_tokens
        .is_some_and(|threshold| threshold < MIN_OPENAI_RESPONSES_COMPACT_THRESHOLD)
    {
        return Err(DomainError::ProviderCompatibility(format!(
            "OpenAI Responses compact_threshold_tokens must be at least {} when set",
            MIN_OPENAI_RESPONSES_COMPACT_THRESHOLD
        )));
    }
    Ok(())
}

fn validate_provider_standalone_compaction(
    compact_threshold_tokens: Option<u32>,
    target_tokens: Option<u32>,
) -> Result<(), DomainError> {
    if compact_threshold_tokens.is_some_and(|tokens| tokens == 0) {
        return Err(DomainError::ProviderCompatibility(
            "provider-standalone compaction compact_threshold_tokens must be greater than 0 when set"
                .to_owned(),
        ));
    }
    if target_tokens.is_some_and(|tokens| tokens == 0) {
        return Err(DomainError::ProviderCompatibility(
            "provider-standalone compaction target_tokens must be greater than 0 when set"
                .to_owned(),
        ));
    }
    Ok(())
}

fn current_config(state: &CoreAgentState) -> Result<&SessionConfig, DomainError> {
    state
        .lifecycle
        .config
        .as_ref()
        .ok_or_else(|| DomainError::InvariantViolation("open session is missing config".to_owned()))
}

fn validate_session_is_idle_for_config_update(state: &CoreAgentState) -> Result<(), DomainError> {
    if state.runs.active.is_some() || !state.runs.queued.is_empty() {
        Err(DomainError::InvariantViolation(
            "session config can only change while no run is active or queued".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn validate_session_api_kind_is_pinned(
    pinned: &ProviderApiKind,
    proposed: &ProviderApiKind,
) -> Result<(), DomainError> {
    if proposed == pinned {
        Ok(())
    } else {
        Err(DomainError::ProviderCompatibility(format!(
            "session provider api kind is pinned to {:?}, got {:?}",
            pinned, proposed
        )))
    }
}

fn validate_active_context_api_kind(
    state: &CoreAgentState,
    api_kind: &ProviderApiKind,
) -> Result<(), DomainError> {
    let _ = (state, api_kind);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(api_kind: ProviderApiKind, compaction: Option<CompactionPolicy>) -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind,
                provider_id: "provider".to_owned(),
                model: "model".to_owned(),
            },
            generation: GenerationConfig::default(),
            limits: LimitsConfig::default(),
            context: ContextConfig { compaction },
            features: FeaturesConfig::default(),
        }
    }

    #[test]
    fn provider_triggered_compaction_rejects_too_small_openai_threshold() {
        let config = config(
            ProviderApiKind::OpenAiResponses,
            Some(CompactionPolicy::ProviderTriggered {
                compact_threshold_tokens: Some(999),
            }),
        );

        let error = config
            .validate()
            .expect_err("threshold below provider minimum must fail");

        assert!(matches!(error, DomainError::ProviderCompatibility(_)));
    }

    #[test]
    fn provider_triggered_compaction_accepts_optional_or_minimum_openai_threshold() {
        for compact_threshold_tokens in [None, Some(MIN_OPENAI_RESPONSES_COMPACT_THRESHOLD)] {
            let config = config(
                ProviderApiKind::OpenAiResponses,
                Some(CompactionPolicy::ProviderTriggered {
                    compact_threshold_tokens,
                }),
            );

            config
                .validate()
                .expect("valid OpenAI provider-triggered compaction");
        }
    }

    #[test]
    fn provider_triggered_compaction_rejects_non_openai_responses_api_kind() {
        let config = config(
            ProviderApiKind::AnthropicMessages,
            Some(CompactionPolicy::ProviderTriggered {
                compact_threshold_tokens: None,
            }),
        );

        let error = config
            .validate()
            .expect_err("provider-triggered compaction is OpenAI Responses only");

        assert!(matches!(error, DomainError::ProviderCompatibility(_)));
    }

    #[test]
    fn provider_standalone_compaction_rejects_zero_values() {
        for compaction in [
            CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens: Some(0),
                target_tokens: Some(128),
            },
            CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens: Some(128),
                target_tokens: Some(0),
            },
        ] {
            let config = config(ProviderApiKind::OpenAiResponses, Some(compaction));

            let error = config
                .validate()
                .expect_err("zero standalone compaction values must fail");

            assert!(matches!(error, DomainError::ProviderCompatibility(_)));
        }
    }

    #[test]
    fn provider_standalone_compaction_accepts_anthropic_messages_api_kind() {
        let config = config(
            ProviderApiKind::AnthropicMessages,
            Some(CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens: None,
                target_tokens: None,
            }),
        );

        config
            .validate()
            .expect("provider-standalone compaction supports Anthropic Messages");
    }

    #[test]
    fn provider_standalone_compaction_accepts_openai_completions_api_kind() {
        let config = config(
            ProviderApiKind::OpenAiCompletions,
            Some(CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens: None,
                target_tokens: None,
            }),
        );

        config
            .validate()
            .expect("provider-standalone compaction supports OpenAI Completions");
    }

    #[test]
    fn web_search_accepts_anthropic_messages() {
        let mut config = config(ProviderApiKind::AnthropicMessages, None);
        config.features.web = Some(WebFeature {
            search: Some(WebSearchFeature::default()),
            ..WebFeature::default()
        });

        config
            .validate()
            .expect("Anthropic web search is supported");
    }

    #[test]
    fn anthropic_web_search_rejects_mixed_domain_filters() {
        let mut config = config(ProviderApiKind::AnthropicMessages, None);
        config.features.web = Some(WebFeature {
            search: Some(WebSearchFeature {
                allowed_domains: Some(vec!["docs.rs".to_owned()]),
                blocked_domains: vec!["example.com".to_owned()],
            }),
            ..WebFeature::default()
        });

        let error = config
            .validate()
            .expect_err("Anthropic should reject mixed domain filters");

        assert!(matches!(error, DomainError::ProviderCompatibility(_)));
    }

    #[test]
    fn web_feature_granting_nothing_is_rejected() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.features.web = Some(WebFeature::default());

        let error = config
            .validate()
            .expect_err("empty web grant must fail validation");

        assert!(matches!(error, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn unsupported_feature_version_is_rejected() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.features.vfs = Some(VfsFeature {
            version: CURRENT_FEATURE_VERSION + 1,
            ..VfsFeature::default()
        });

        let error = config
            .validate()
            .expect_err("unknown feature version must fail validation");

        assert!(matches!(error, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn subagent_deadline_accepts_24_hours_and_rejects_larger_values() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.features.subagents = Some(SubagentsFeature {
            agents: vec![SubagentAgentConfig {
                profile_id: "reviewer".to_owned(),
            }],
            limits: SubagentLimits {
                deadline_ms: SUBAGENT_DEADLINE_CEILING_MS,
                ..SubagentLimits::default()
            },
            ..SubagentsFeature::default()
        });

        config
            .validate()
            .expect("24-hour deadline must be accepted");

        config
            .features
            .subagents
            .as_mut()
            .expect("subagents feature")
            .limits
            .deadline_ms = SUBAGENT_DEADLINE_CEILING_MS + 1;
        assert!(matches!(
            config.validate(),
            Err(DomainError::InvariantViolation(_))
        ));
    }

    #[test]
    fn domain_working_directories_and_source_overrides_validate_independently() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.features.environments = Some(EnvironmentsFeature {
            prompts: Some(Default::default()),
            skills: Some(EnvironmentSkillsConfig {
                roots: Some(vec!["./custom".into()]),
            }),
            environments: vec![EnvironmentAttachment {
                environment_id: "env_a".into(),
                default: true,
                access: EnvironmentAccess::Exec,
                working_directory: Some("/project".into()),
            }],
            ..Default::default()
        });
        config.features.vfs = Some(VfsFeature {
            working_directory: Some("/".into()),
            ..Default::default()
        });
        config.validate().unwrap();
        config.features.environments.as_mut().unwrap().environments[0].working_directory =
            Some("relative".into());
        assert!(config.validate().is_err());
        config.features.environments.as_mut().unwrap().environments[0].working_directory = None;
        config.features.environments.as_mut().unwrap().prompts = Some(EnvironmentPromptsConfig {
            roots: Some(vec![]),
        });
        assert!(config.validate().is_err());
        config.features.environments.as_mut().unwrap().prompts = None;
        config.features.vfs.as_mut().unwrap().working_directory = Some("/unlinked".into());
        assert!(config.validate().is_err());
    }

    #[test]
    fn empty_vfs_source_blocks_enable_defaults_without_links() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.features.vfs = Some(VfsFeature {
            skills: Some(VfsSkillsConfig::default()),
            prompts: Some(VfsPromptsConfig::default()),
            ..Default::default()
        });
        config.validate().unwrap();
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["features"]["vfs"]["skills"], serde_json::json!({}));
        assert_eq!(json["features"]["vfs"]["prompts"], serde_json::json!({}));
        assert_eq!(
            serde_json::from_value::<SessionConfig>(json).unwrap(),
            config
        );
    }

    #[test]
    fn explicit_empty_source_roots_are_rejected() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.features.vfs = Some(VfsFeature {
            skills: Some(VfsSkillsConfig {
                roots: Some(Vec::new()),
            }),
            ..VfsFeature::default()
        });

        let error = config
            .validate()
            .expect_err("explicit empty roots must fail validation");

        assert!(matches!(error, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn vfs_skills_require_linked_roots_and_are_independent_of_environment_skills() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.features.environments = Some(EnvironmentsFeature {
            skills: Some(EnvironmentSkillsConfig::default()),
            ..Default::default()
        });
        config.features.vfs = Some(VfsFeature {
            workspaces: vec![WorkspaceAttachment {
                path: "/workspace".into(),
                target: WorkspaceAttachmentTarget::Workspace {
                    workspace_id: "project".into(),
                },
                access: WorkspaceAccess::Read,
            }],
            ..Default::default()
        });
        config.validate().unwrap();
        assert!(config.features.vfs.as_ref().unwrap().skills.is_none());
        for roots in [
            vec![],
            vec!["relative"],
            vec!["/outside"],
            vec!["/workspace/../skills"],
            vec!["/workspace/skills", "/workspace/skills"],
        ] {
            config.features.vfs.as_mut().unwrap().skills = Some(VfsSkillsConfig {
                roots: Some(roots.into_iter().map(String::from).collect()),
            });
            assert!(matches!(
                config.validate(),
                Err(DomainError::InvariantViolation(_))
            ));
        }
        config.features.vfs.as_mut().unwrap().skills = Some(VfsSkillsConfig {
            roots: Some(vec!["/workspace/team-skills".into()]),
        });
        config.validate().unwrap();
        config.features.environments = None;
        config.validate().unwrap();
        assert_eq!(
            serde_json::from_str::<VfsSkillsConfig>("{}").unwrap(),
            VfsSkillsConfig::default()
        );
    }

    #[test]
    fn workspace_attachments_validate_topology_access_and_explicit_roots() {
        let workspace = WorkspaceAttachment {
            path: "/workspace".to_owned(),
            target: WorkspaceAttachmentTarget::Workspace {
                workspace_id: "workspace_1".to_owned(),
            },
            access: WorkspaceAccess::Edit,
        };
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.features.vfs = Some(VfsFeature {
            workspaces: vec![workspace.clone()],
            prompts: Some(VfsPromptsConfig {
                roots: Some(vec!["/workspace/.agents/prompts".to_owned()]),
            }),
            ..VfsFeature::default()
        });
        config
            .validate()
            .expect("valid workspace attachment topology");

        config
            .features
            .vfs
            .as_mut()
            .unwrap()
            .workspaces
            .push(WorkspaceAttachment {
                path: "/workspace/nested".to_owned(),
                ..workspace.clone()
            });
        assert!(matches!(
            config.validate(),
            Err(DomainError::InvariantViolation(_))
        ));

        config.features.vfs.as_mut().unwrap().workspaces = vec![WorkspaceAttachment {
            path: "/skills".to_owned(),
            target: WorkspaceAttachmentTarget::Snapshot {
                snapshot_ref: format!("sha256:{}", "a".repeat(64)),
            },
            access: WorkspaceAccess::Edit,
        }];
        assert!(matches!(
            config.validate(),
            Err(DomainError::InvariantViolation(_))
        ));

        config.features.vfs.as_mut().unwrap().workspaces = vec![workspace];
        config.features.vfs.as_mut().unwrap().prompts = Some(VfsPromptsConfig {
            roots: Some(vec!["/outside/prompts".to_owned()]),
        });
        assert!(matches!(
            config.validate(),
            Err(DomainError::InvariantViolation(_))
        ));
    }

    #[test]
    fn empty_reasoning_effort_is_rejected() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.generation.reasoning_effort = Some("  ".to_owned());

        let error = config
            .validate()
            .expect_err("blank reasoning effort must fail validation");

        assert!(matches!(error, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn mcp_feature_requires_unique_nonempty_server_ids() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.features.mcp = Some(McpFeature::default());
        config.validate().expect("an empty attachment list is valid");

        let link = McpServerLink {
            server_id: "linear".to_owned(),
            tools: None,
        };
        let mut duplicated = config.clone();
        duplicated.features.mcp = Some(McpFeature {
            servers: vec![link.clone(), link.clone()],
            ..McpFeature::default()
        });
        let error = duplicated
            .validate()
            .expect_err("duplicate server links must fail");
        assert!(matches!(error, DomainError::InvariantViolation(_)));

        let mut blank = config.clone();
        blank.features.mcp.as_mut().unwrap().servers.push(McpServerLink {
            server_id: " ".to_owned(),
            tools: None,
        });
        assert!(matches!(blank.validate(), Err(DomainError::InvariantViolation(_))));

        for tools in [vec![], vec![""], vec!["search", "search"]] {
            let mut narrowed = config.clone();
            narrowed.features.mcp = Some(McpFeature {
                servers: vec![McpServerLink {
                    tools: Some(tools.into_iter().map(String::from).collect()),
                    ..link.clone()
                }],
                ..McpFeature::default()
            });
            assert!(matches!(
                narrowed.validate(),
                Err(DomainError::InvariantViolation(_))
            ));
        }
        let mut narrowed = config;
        narrowed.features.mcp = Some(McpFeature {
            servers: vec![McpServerLink {
                tools: Some(vec!["search".to_owned()]),
                ..link
            }],
            ..McpFeature::default()
        });
        narrowed
            .validate()
            .expect("a non-empty unique subset is valid");
    }

    #[test]
    fn mcp_empty_attachments_survive_lifecycle_replay() {
        use crate::core::components::lifecycle::{Event, apply_event};

        let mut empty = config(ProviderApiKind::OpenAiResponses, None);
        empty.features.mcp = Some(McpFeature::default());
        let mut attached = empty.clone();
        attached.features.mcp.as_mut().unwrap().servers.push(McpServerLink {
            server_id: "catalog".to_owned(),
            tools: None,
        });
        let events = [
            Event::Opened { config: empty.clone() },
            Event::ConfigChanged { config: attached, revision: 1 },
            Event::ConfigChanged { config: empty.clone(), revision: 2 },
        ];
        let mut original = CoreAgentState::new();
        let mut replayed = CoreAgentState::new();
        for event in events {
            apply_event(&mut original, &event).expect("apply lifecycle event");
            let bytes = serde_json::to_vec(&event).expect("encode event");
            let decoded: Event = serde_json::from_slice(&bytes).expect("decode event");
            apply_event(&mut replayed, &decoded).expect("replay lifecycle event");
        }
        assert_eq!(original, replayed);
        assert_eq!(replayed.lifecycle.config, Some(empty.clone()));
        assert_eq!(replayed.lifecycle.config_revision, 2);
        assert_eq!(serde_json::to_value(empty).unwrap()["features"]["mcp"]["servers"], serde_json::json!([]));
    }

    #[test]
    fn environment_attachments_validate_identity_default_and_working_directory() {
        fn attachment(id: &str) -> EnvironmentAttachment {
            EnvironmentAttachment {
                environment_id: id.to_owned(),
                default: false,
                access: EnvironmentAccess::Read,
                working_directory: None,
            }
        }
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.features.environments = Some(EnvironmentsFeature {
            environments: vec![
                EnvironmentAttachment {
                    default: true,
                    access: EnvironmentAccess::Jobs,
                    working_directory: Some("/srv/app".to_owned()),
                    ..attachment("env_a")
                },
                attachment("env_b"),
            ],
            ..EnvironmentsFeature::default()
        });
        config.validate().expect("two attachments with one default");
        let feature = config.features.environments.as_ref().unwrap();
        assert_eq!(
            feature
                .default_attachment()
                .map(|a| a.environment_id.as_str()),
            Some("env_a")
        );
        assert_eq!(feature.tool_access(), Some(EnvironmentAccess::Jobs));
        assert!(feature.is_attached("env_b"));
        assert!(!feature.is_attached("env_c"));

        let invalid = [
            vec![attachment("env_a"), attachment("env_a")],
            vec![
                EnvironmentAttachment {
                    default: true,
                    ..attachment("env_a")
                },
                EnvironmentAttachment {
                    default: true,
                    ..attachment("env_b")
                },
            ],
            vec![attachment(" ")],
            vec![attachment("env/bad")],
            vec![attachment("env bad")],
            vec![attachment("-invalid-start")],
            vec![attachment(&"a".repeat(129))],
            vec![EnvironmentAttachment {
                working_directory: Some("relative".to_owned()),
                ..attachment("env_a")
            }],
        ];
        for environments in invalid {
            config.features.environments = Some(EnvironmentsFeature {
                environments,
                ..EnvironmentsFeature::default()
            });
            assert!(matches!(
                config.validate(),
                Err(DomainError::InvariantViolation(_))
            ));
        }

        config.features.environments = Some(EnvironmentsFeature::default());
        config
            .validate()
            .expect("an environments grant without attachments is valid");
        assert_eq!(
            config.features.environments.as_ref().unwrap().tool_access(),
            None
        );
    }

    #[test]
    fn access_ladders_are_ordered_and_implied() {
        assert!(WorkspaceAccess::Edit > WorkspaceAccess::Read);
        assert!(WorkspaceAccess::Edit.allows_edit());
        assert!(!WorkspaceAccess::Read.allows_edit());
        assert!(EnvironmentAccess::Jobs > EnvironmentAccess::Exec);
        assert!(EnvironmentAccess::Exec > EnvironmentAccess::Edit);
        assert!(EnvironmentAccess::Edit > EnvironmentAccess::Read);
        assert!(EnvironmentAccess::Exec.allows_edit());
        assert!(EnvironmentAccess::Exec.allows_exec());
        assert!(!EnvironmentAccess::Exec.allows_jobs());
        assert!(EnvironmentAccess::Jobs.allows_jobs());
        assert!(!EnvironmentAccess::Read.allows_edit());
        assert_eq!(EnvironmentAccess::Exec.describe(), "read, edit, exec");
        assert_eq!(
            serde_json::to_value(EnvironmentAccess::Jobs).unwrap(),
            serde_json::json!("jobs")
        );
    }

    #[test]
    fn minimal_config_serializes_to_model_only() {
        let config = config(ProviderApiKind::OpenAiResponses, None);

        let value = serde_json::to_value(&config).expect("serialize");

        let object = value.as_object().expect("config must serialize as object");
        assert_eq!(object.keys().collect::<Vec<_>>(), vec!["model"]);
    }

    #[test]
    fn omitted_feature_version_decodes_and_reserializes_pinned() {
        let feature: VfsFeature = serde_json::from_value(serde_json::json!({}))
            .expect("empty vfs grant decodes with defaults");
        assert_eq!(feature.version, CURRENT_FEATURE_VERSION);
        assert!(feature.workspaces.is_empty());
        assert_eq!(feature.tool_access(), None);

        let value = serde_json::to_value(&feature).expect("serialize");
        assert_eq!(
            value,
            serde_json::json!({ "version": CURRENT_FEATURE_VERSION })
        );
    }

    #[test]
    fn environment_grant_is_default_off_with_no_attachments() {
        let feature: EnvironmentsFeature = serde_json::from_value(serde_json::json!({}))
            .expect("empty environment grant decodes with defaults");

        assert!(!feature.selection);
        assert!(feature.environments.is_empty());
        assert_eq!(
            serde_json::to_value(feature).expect("serialize"),
            serde_json::json!({
                "version": CURRENT_FEATURE_VERSION,
                "selection": false,
            })
        );
    }

    #[test]
    fn vfs_tool_access_is_the_widest_attachment_grant() {
        let read = WorkspaceAttachment {
            path: "/ref".to_owned(),
            target: WorkspaceAttachmentTarget::Workspace {
                workspace_id: "ref".to_owned(),
            },
            access: WorkspaceAccess::Read,
        };
        let edit = WorkspaceAttachment {
            path: "/workspace".to_owned(),
            target: WorkspaceAttachmentTarget::Workspace {
                workspace_id: "app".to_owned(),
            },
            access: WorkspaceAccess::Edit,
        };
        let feature = VfsFeature {
            workspaces: vec![read.clone()],
            ..VfsFeature::default()
        };
        assert_eq!(feature.tool_access(), Some(WorkspaceAccess::Read));
        let feature = VfsFeature {
            workspaces: vec![read, edit],
            ..VfsFeature::default()
        };
        assert_eq!(feature.tool_access(), Some(WorkspaceAccess::Edit));
    }

    #[test]
    fn sparse_config_round_trips() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.generation.reasoning_effort = Some("high".to_owned());
        config.features.vfs = Some(VfsFeature {
            prompts: Some(VfsPromptsConfig::default()),
            ..VfsFeature::default()
        });
        config.features.web = Some(WebFeature {
            fetch: Some(WebFetchFeature::default()),
            ..WebFeature::default()
        });

        let value = serde_json::to_value(&config).expect("serialize");
        let decoded: SessionConfig = serde_json::from_value(value).expect("deserialize");

        assert_eq!(decoded, config);
    }
}
