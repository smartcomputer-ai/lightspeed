use serde::{Deserialize, Serialize};

use crate::{
    CoreAgentState, DomainError, ModelSelection, ProviderApiKind, ProviderParams,
    RemoteMcpApprovalPolicy, ToolChoice,
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
        validate_generation(&self.generation, &self.model.api_kind)?;
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
    /// "high", "xhigh", "ultra"). The engine carries it opaquely; the LLM
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
    pub messaging: Option<MessagingFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet: Option<FleetFeature>,
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

/// Grants the session virtual filesystem: mounts may be attached and the VFS
/// catalog is surfaced to the session. The sub-blocks grant the agent tool
/// surface and prompt/skill sourcing independently — `{}` grants a VFS with
/// no tools and no sourcing. Sourcing from mounted environments is a later,
/// environment-specific concern and does not live here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    /// Agent-facing filesystem tool surface: absent = no fs tools (a
    /// sourcing-only VFS is valid); `read_only` installs the read surface;
    /// `edit` adds the write tools. Per-path writability is defined and
    /// enforced by each mount's own access — this field shapes which tools
    /// exist, not path permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<VfsToolSurface>,
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
            tools: None,
            prompts: None,
            skills: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VfsToolSurface {
    ReadOnly,
    Edit,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsPromptsConfig {
    /// VFS roots to source prompts from. `None` means the conventional
    /// roots; an explicit list must be non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsSkillsConfig {
    /// VFS roots to source skills from. `None` means the conventional
    /// roots; an explicit list must be non-empty.
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

/// Grants the messaging toolset (message_send/react/edit/noop) for sessions
/// bound to a chat channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagingFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
}

impl Default for MessagingFeature {
    fn default() -> Self {
        Self {
            version: CURRENT_FEATURE_VERSION,
        }
    }
}

/// Grants the Fleet subagent control plane
/// (agent_spawn/send/read/list/cancel and profile_list/read).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "FleetProfilesConfig::is_default")]
    pub profiles: FleetProfilesConfig,
    #[serde(default, skip_serializing_if = "FleetSpawnConfig::is_default")]
    pub spawn: FleetSpawnConfig,
}

impl Default for FleetFeature {
    fn default() -> Self {
        Self {
            version: CURRENT_FEATURE_VERSION,
            profiles: FleetProfilesConfig::default(),
            spawn: FleetSpawnConfig::default(),
        }
    }
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

/// Grants attaching/activating session environments and their process/job
/// tool surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentsFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<String>>,
}

impl Default for EnvironmentsFeature {
    fn default() -> Self {
        Self {
            version: CURRENT_FEATURE_VERSION,
            providers: None,
        }
    }
}

/// Grants remote MCP tools by declaring linked servers from the universe MCP
/// catalog. Reconciliation into tool specs happens in the runtime
/// materialization layer, not in the engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    /// Must be non-empty with unique server ids; omit the feature instead of
    /// linking zero servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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

/// A linked catalog server with optional per-session overrides; `None`
/// fields defer to the catalog record's defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerLink {
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<RemoteMcpApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    /// Universe-scoped auth grant to authenticate against the server. The
    /// engine carries the reference opaquely; compatibility with the
    /// server's auth policy is validated at the admission boundary against
    /// the catalog, and the token broker resolves it at request time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_grant_id: Option<String>,
}

/// Fleet profile visibility policy for spawn/list/read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetProfilesConfig {
    /// None means all named profiles are visible/spawnable; Some(empty) means none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub inline: bool,
}

impl Default for FleetProfilesConfig {
    fn default() -> Self {
        Self {
            allow: None,
            deny: Vec::new(),
            inline: true,
        }
    }
}

impl FleetProfilesConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn named_profile_allowed(&self, profile_id: &str) -> bool {
        let allowed = self
            .allow
            .as_ref()
            .is_none_or(|allow| allow.iter().any(|allowed| allowed == profile_id));
        let denied = self.deny.iter().any(|denied| denied == profile_id);
        allowed && !denied
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetSpawnConfig {
    /// None means all spawn bases are allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bases: Option<Vec<FleetSpawnBase>>,
}

impl FleetSpawnConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn base_allowed(&self, base: FleetSpawnBase) -> bool {
        self.bases
            .as_ref()
            .is_none_or(|bases| bases.contains(&base))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetSpawnBase {
    #[serde(rename = "self")]
    Self_,
    Session,
    Profile,
}

fn default_feature_version() -> u32 {
    CURRENT_FEATURE_VERSION
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
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
    api_kind: &ProviderApiKind,
) -> Result<(), DomainError> {
    let _ = api_kind;
    if generation
        .reasoning_effort
        .as_ref()
        .is_some_and(|effort| effort.trim().is_empty())
    {
        return Err(DomainError::InvariantViolation(
            "reasoning_effort must be a non-empty string when set".to_owned(),
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
        if let Some(prompts) = &vfs.prompts {
            validate_source_roots("vfs prompts", prompts.roots.as_deref())?;
        }
        if let Some(skills) = &vfs.skills {
            validate_source_roots("vfs skills", skills.roots.as_deref())?;
        }
    }
    if let Some(web) = &features.web {
        validate_feature_version("web", web.version)?;
        validate_web_feature(web, api_kind)?;
    }
    if let Some(messaging) = &features.messaging {
        validate_feature_version("messaging", messaging.version)?;
    }
    if let Some(fleet) = &features.fleet {
        validate_feature_version("fleet", fleet.version)?;
    }
    if let Some(timers) = &features.timers {
        validate_feature_version("timers", timers.version)?;
    }
    if let Some(environments) = &features.environments {
        validate_feature_version("environments", environments.version)?;
    }
    if let Some(mcp) = &features.mcp {
        validate_feature_version("mcp", mcp.version)?;
        validate_mcp_feature(mcp)?;
    }
    Ok(())
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

fn validate_source_roots(feature: &str, roots: Option<&[String]>) -> Result<(), DomainError> {
    let Some(roots) = roots else {
        return Ok(());
    };
    if roots.is_empty() {
        return Err(DomainError::InvariantViolation(format!(
            "explicit {} roots must be non-empty; omit roots for the conventional defaults",
            feature
        )));
    }
    if roots.iter().any(|root| root.trim().is_empty()) {
        return Err(DomainError::InvariantViolation(format!(
            "{} roots must not contain empty paths",
            feature
        )));
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
        if api_kind != &ProviderApiKind::OpenAiResponses {
            return Err(DomainError::ProviderCompatibility(format!(
                "web search requires OpenAI Responses api kind, got {:?}",
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
    }
    Ok(())
}

fn validate_mcp_feature(mcp: &McpFeature) -> Result<(), DomainError> {
    if mcp.servers.is_empty() {
        return Err(DomainError::InvariantViolation(
            "mcp feature links zero servers; omit the feature instead".to_owned(),
        ));
    }
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
        if link
            .auth_grant_id
            .as_ref()
            .is_some_and(|grant_id| grant_id.trim().is_empty())
        {
            return Err(DomainError::InvariantViolation(format!(
                "mcp server {} auth_grant_id must be non-empty when set",
                link.server_id
            )));
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
            ProviderApiKind::OpenAiResponses | ProviderApiKind::AnthropicMessages,
        ) => validate_provider_standalone_compaction(*compact_threshold_tokens, *target_tokens),
        (Some(CompactionPolicy::ProviderTriggered { .. }), api_kind) => {
            Err(DomainError::ProviderCompatibility(format!(
                "provider-triggered compaction requires OpenAI Responses api kind, got {:?}",
                api_kind
            )))
        }
        (Some(CompactionPolicy::ProviderStandalone { .. }), api_kind) => {
            Err(DomainError::ProviderCompatibility(format!(
                "provider-standalone compaction requires OpenAI Responses or Anthropic Messages api kind, got {:?}",
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
    fn provider_standalone_compaction_rejects_unsupported_api_kind() {
        let config = config(
            ProviderApiKind::OpenAiCompletions,
            Some(CompactionPolicy::ProviderStandalone {
                compact_threshold_tokens: None,
                target_tokens: None,
            }),
        );

        let error = config
            .validate()
            .expect_err("provider-standalone compaction has no OpenAI Completions adapter");

        assert!(matches!(error, DomainError::ProviderCompatibility(_)));
    }

    #[test]
    fn web_search_requires_openai_responses() {
        let mut config = config(ProviderApiKind::AnthropicMessages, None);
        config.features.web = Some(WebFeature {
            search: Some(WebSearchFeature::default()),
            ..WebFeature::default()
        });

        let error = config
            .validate()
            .expect_err("web search should reject Anthropic");

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
    fn empty_reasoning_effort_is_rejected() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.generation.reasoning_effort = Some("  ".to_owned());

        let error = config
            .validate()
            .expect_err("blank reasoning effort must fail validation");

        assert!(matches!(error, DomainError::InvariantViolation(_)));
    }

    #[test]
    fn mcp_feature_requires_unique_nonempty_servers() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.features.mcp = Some(McpFeature::default());
        let error = config
            .validate()
            .expect_err("zero linked servers must fail");
        assert!(matches!(error, DomainError::InvariantViolation(_)));

        let link = McpServerLink {
            server_id: "linear".to_owned(),
            allowed_tools: None,
            approval: None,
            defer_loading: None,
            auth_grant_id: None,
        };
        let mut duplicated = config;
        duplicated.features.mcp = Some(McpFeature {
            servers: vec![link.clone(), link],
            ..McpFeature::default()
        });
        let error = duplicated
            .validate()
            .expect_err("duplicate server links must fail");
        assert!(matches!(error, DomainError::InvariantViolation(_)));
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
        assert_eq!(feature.tools, None);

        let value = serde_json::to_value(&feature).expect("serialize");
        assert_eq!(
            value,
            serde_json::json!({ "version": CURRENT_FEATURE_VERSION })
        );
    }

    #[test]
    fn vfs_tool_surface_grant_decodes() {
        let feature: VfsFeature = serde_json::from_value(serde_json::json!({ "tools": "edit" }))
            .expect("vfs tool surface grant decodes");

        assert_eq!(feature.tools, Some(VfsToolSurface::Edit));
    }

    #[test]
    fn sparse_config_round_trips() {
        let mut config = config(ProviderApiKind::OpenAiResponses, None);
        config.generation.reasoning_effort = Some("high".to_owned());
        config.features.vfs = Some(VfsFeature {
            tools: Some(VfsToolSurface::Edit),
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
