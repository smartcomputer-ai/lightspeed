use super::*;

/// Longest admitted close-relative automatic-deletion duration: 100 years.
pub const MAX_SESSION_DELETE_AFTER_CLOSE_MS: u64 = 100 * 365 * 24 * 60 * 60 * 1_000;

fn deserialize_optional_nullable_delete_after_close_ms<'de, D>(
    deserializer: D,
) -> Result<Option<Option<u64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<u64>::deserialize(deserializer).map(Some)
}

fn optional_nullable_delete_after_close_ms_schema(
    _: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["integer", "null"],
        "format": "uint64",
        "minimum": 1,
        "maximum": 3153600000000_u64
    })
}

/// Creation-time override for the environment intent carried by a profile.
/// Absence uses the profile unchanged; `none` suppresses its environment
/// intent, while `existing` activates the specified universe environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SessionEnvironmentOverride {
    None {},
    Existing { environment_id: EnvironmentId },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionStartParams {
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Descriptive key/value metadata, applied only when the session is
    /// first created: at most 32 entries, keys 1..=64 bytes, values 1..=256
    /// bytes, no control characters, no `lightspeed.` prefix. It never
    /// affects routing, authority, or selection; filter on it with
    /// `session/list`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<SessionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileSource>,
    /// Optional creation-time override for the selected profile's environment
    /// intent. Omit to use the profile's intent unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<SessionEnvironmentOverride>,
    /// Root-owned automatic deletion measured from close. Absent inherits a
    /// profile default, explicit null keeps the tree, and a duration overrides
    /// the profile.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_nullable_delete_after_close_ms",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(schema_with = "optional_nullable_delete_after_close_ms_schema")]
    pub delete_after_close_ms: Option<Option<u64>>,
}

/// Creation request for a session with immutable workflow ownership and
/// workflow-backed tools.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedSessionStartParams {
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Descriptive key/value metadata with the same bounds as
    /// `session/start`; applied only when the session is first created.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<SessionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileSource>,
    /// Optional creation-time override for the selected profile's environment
    /// intent. Omit to use the profile's intent unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<SessionEnvironmentOverride>,
    /// Root-owned automatic deletion measured from close. Absent inherits a
    /// profile default, explicit null keeps the tree, and a duration overrides
    /// the profile.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_nullable_delete_after_close_ms",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(schema_with = "optional_nullable_delete_after_close_ms_schema")]
    pub delete_after_close_ms: Option<Option<u64>>,
    /// Immutable workflow tools admitted only when the session is first
    /// created. This document is not part of `SessionConfig` and cannot be
    /// changed through `session/config/put`.
    pub workflow_tools: ManagedSessionWorkflowToolsInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedSessionWorkflowToolsInput {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_controller: Option<WorkflowEndpointInput>,
    pub tools: Vec<WorkflowToolDeclarationInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowEndpointInput {
    pub workflow_id: String,
    pub workflow_kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowToolDeclarationInput {
    pub definition: WorkflowToolDefinitionInput,
    pub target: WorkflowToolTargetInput,
    pub completion: WorkflowToolCompletionInput,
}

/// Lifecycle target of a workflow-backed tool. Bound tools deliver to an
/// existing execution; start tools create an execution from an immutable,
/// CAS-backed recipe for every invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WorkflowToolTargetInput {
    Bound {
        receiver: WorkflowEndpointInput,
        dispatch: BoundWorkflowToolDispatchInput,
    },
    Start {
        start: WorkflowStartRefInput,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
/// How an invocation of a bound workflow tool reaches its receiver.
pub enum BoundWorkflowToolDispatchInput {
    /// The receiver consumes the invocation from the authorized session log.
    Pull,
    /// The runtime durably emits the invocation to the receiver workflow.
    Push,
}

/// Opaque reference to a workflow-substrate recipe already stored through
/// the blob API. The fingerprint authenticates the exact recipe bytes used
/// by the workflow-start adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowStartRefInput {
    pub recipe_format: u32,
    pub revision: u32,
    pub recipe_ref: String,
    pub recipe_fingerprint: String,
}

/// Completion contract for one workflow-tool invocation. Joined tools park
/// the original call on one runtime-owned reply; Promises exposes handles for
/// model-controlled concurrency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WorkflowToolCompletionInput {
    Accepted,
    Joined {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_schema_ref: Option<String>,
        deadline_after_ms: u64,
    },
    Promises {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_schema_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deadline_after_ms: Option<u64>,
        max_promises: u32,
        key_source: WorkflowToolCompletionKeySourceInput,
    },
}

/// Declarative promise-key derivation over schema-validated tool arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WorkflowToolCompletionKeySourceInput {
    /// The single reserved `reply` key.
    Reply,
    /// A JSON Pointer to an array of unique completion-key strings.
    StringArray { pointer: String },
    /// A JSON Pointer to an array of objects; the named string field of
    /// every item is its completion key, so the model's own name for a work
    /// item keys that item's promise.
    ArrayItemField { pointer: String, field: String },
    /// A JSON Pointer to an array; keys are `prefix` joined with each item's
    /// zero-based index.
    ArrayIndices { pointer: String, prefix: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowToolDefinitionInput {
    pub tool_id: String,
    pub revision: u32,
    pub semantic_type: String,
    pub tool: WorkflowToolSpecInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowToolSpecInput {
    /// Function name exposed to the model and used as its registry identity.
    pub name: String,
    pub kind: WorkflowToolKindInput,
    #[serde(default)]
    pub parallelism: ToolParallelismView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WorkflowToolKindInput {
    Function {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description_ref: Option<String>,
        input_schema_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_schema_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_options_ref: Option<String>,
    },
}

/// Current version of every feature block. Omitted versions on input decode
/// to this value; documents read back from a session always carry the pinned
/// version.
pub const CURRENT_FEATURE_VERSION: u32 = 1;

fn default_feature_version() -> u32 {
    CURRENT_FEATURE_VERSION
}

/// Declared session configuration document.
///
/// Sparse and capability-oriented: an omitted section means defaults, an
/// absent feature is not granted (no tools, no access). The document is
/// replaced whole via `session/config/put`; reads return exactly the stored
/// document, so read-modify-write round-trips.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionConfig {
    /// Absent on input means the deployment default model. Documents read
    /// back from a session always carry the model; the provider api kind is
    /// pinned for the session's lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<LimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<FeaturesConfig>,
}

/// Turn-shaping defaults applied to every LLM generation. Per-run overrides
/// ride `session/runs/start`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Reasoning effort tier as a provider-native string (e.g. "none",
    /// "high", "xhigh", "max"); validated against the session's provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Whether the model may call several tools in one turn; absent leaves
    /// the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_use: Option<bool>,
    /// Provider processing class. In session/profile config this becomes the
    /// default for every run; in run config it overrides that run. Currently
    /// supported only by the built-in OpenAI provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing_tier: Option<ModelProcessingTier>,
}

/// Provider processing class used by session defaults and per-run overrides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ModelProcessingTier {
    Standard,
    Fast,
    Flex,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ToolChoice {
    Auto,
    None,
    RequiredAny,
    Specific { tool_id: String },
}

/// Run budget defaults enforced by the engine drive loop.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LimitsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionPolicy>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "mode"
)]
pub enum CompactionPolicy {
    Disabled,
    ProviderTriggered {
        #[serde(
            default,
            alias = "compact_threshold_tokens",
            skip_serializing_if = "Option::is_none"
        )]
        compact_threshold_tokens: Option<u32>,
    },
    ProviderStandalone {
        #[serde(
            default,
            alias = "compact_threshold_tokens",
            skip_serializing_if = "Option::is_none"
        )]
        compact_threshold_tokens: Option<u32>,
        #[serde(
            default,
            alias = "target_tokens",
            skip_serializing_if = "Option::is_none"
        )]
        target_tokens: Option<u32>,
    },
}

/// Capability grants. An absent feature is not granted; `{}` grants it with
/// defaults. Every block carries a behavior `version` that pins semantics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

/// Grants the session virtual filesystem. Workspace links declare the
/// session-visible namespace and the VFS catalog is surfaced. Sub-grants are independent; `{}` grants a VFS with
/// no tools and no sourcing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VfsFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    /// Absolute VFS tool working directory; absent uses /.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Catalog resources exposed in the session's workspace namespace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_links: Vec<WorkspaceLink>,
    /// Agent-facing filesystem tool surface; absent = no fs tools. Per-path
    /// writability is defined by each workspace link's own access.
    /// With the environments feature granted, `readOnly` also exposes
    /// `vfs_materialize`; `edit` additionally exposes `vfs_capture`.
    /// Prompt/skill sourcing alone does not grant transfer tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<VfsToolSurface>,
    /// Prompt-instruction sourcing from the VFS. Absent disables loading;
    /// an empty block discovers conventional linked roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<VfsPromptsConfig>,
    /// Independent VFS skill discovery. Absent disables discovery and removes
    /// its runtime catalog; an empty block discovers conventional linked roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<VfsSkillsConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceLink {
    pub path: String,
    pub target: WorkspaceLinkTarget,
    pub access: WorkspaceLinkAccess,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WorkspaceLinkTarget {
    Workspace { workspace_id: String },
    Snapshot { snapshot_ref: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceLinkAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum VfsToolSurface {
    ReadOnly,
    Edit,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VfsPromptsConfig {
    /// Absent searches .agents/prompts and .lightspeed/prompts beneath each
    /// workspace link. Explicit roots replace these defaults and must be
    /// non-empty absolute paths contained in workspace links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VfsSkillsConfig {
    /// Absent searches .agents/skills and .lightspeed/skills beneath each
    /// workspace link. Explicit roots replace these defaults and must be
    /// non-empty absolute paths contained in workspace links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub roots: Option<Vec<String>>,
}

/// Grants network access through the web toolset; `fetch` and `search` are
/// independently granted, and a web block granting neither is rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch: Option<WebFetchFeature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<WebSearchFeature>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebFetchFeature {}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchFeature {
    /// Absent means all domains; an explicit list must be non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_domains: Vec<String>,
}

/// Grants sub-agent delegation: `agent_run` (joined, result inline) and
/// `agent_spawn` (promise, joined with `await`) over the listed agent
/// profiles. Limits are root-scoped and attenuating: every descendant of a
/// root session counts against the root, and a nested grant can narrow but
/// never widen the limits pinned on its origin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentsFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    /// The agent menu. Every id must name an existing profile; the model
    /// picks by id and reads descriptions from the sub-agent catalog.
    pub agents: Vec<SubagentAgentRef>,
    /// A child at depth `d` may spawn only while `d + 1 <= maxDepth`.
    #[serde(default = "default_subagent_max_depth")]
    pub max_depth: u32,
    /// Lifetime total of sessions ever created under the root.
    #[serde(default = "default_subagent_max_descendants")]
    pub max_descendants: u32,
    /// Open sessions under the root at any time, excluding the root.
    #[serde(default = "default_subagent_max_concurrent")]
    pub max_concurrent: u32,
    /// Per-child run deadline in milliseconds; at most the execution
    /// ceiling of 24 hours.
    #[serde(default = "default_subagent_deadline_ms")]
    pub deadline_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentAgentRef {
    pub profile_id: ProfileId,
}

fn default_subagent_max_depth() -> u32 {
    2
}

fn default_subagent_max_descendants() -> u32 {
    16
}

fn default_subagent_max_concurrent() -> u32 {
    4
}

fn default_subagent_deadline_ms() -> u64 {
    60 * 60 * 1_000
}

/// Grants timer promises through the sleep tool plus the base concurrency
/// tools (await/cancel/detach).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimersFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
}

/// Grants active session environments. Filesystem tools, commands, selection,
/// durable jobs, prompts, and skills are independent, default-off sub-grants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentsFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    /// Filesystem tool surface. Absent installs no filesystem tools; sources
    /// remain independent. Read-only does not restrict commands or durable jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<EnvironmentToolSurface>,
    /// Grants command execution and process continuation. Commands may modify
    /// files even when filesystem tools are read-only or disabled.
    #[serde(default)]
    pub commands: bool,
    /// Absolute machine working directory for file tools, commands, jobs, and sources; absent uses the endpoint default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Absent means every registered provider is allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<EnvironmentProviderId>>,
    /// Registration keys whose registered environments the session may
    /// list and activate; absent means every key. Independent of
    /// `providers`: each list scopes its own environment source, and
    /// external environments pass only when neither list is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_keys: Option<Vec<EnvironmentRegistrationKeyId>>,
    /// Exposes `environment_list`, `environment_activate`, and
    /// `environment_deactivate` to the model. `environment_read` is available
    /// whenever environments are enabled, and external API/profile activation
    /// remains available when this is false.
    #[serde(default)]
    pub selection_tools: bool,
    /// Grants the advanced durable-job tool surface. The workflow binding is
    /// installed for the session when granted; invocations still require an
    /// active, ready environment with matching job capabilities.
    #[serde(default)]
    pub jobs: bool,
    /// Independent environment prompt loading; absent disables sourced instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<EnvironmentPromptsConfig>,
    /// Independent environment skill discovery. Absent disables discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<EnvironmentSkillsConfig>,
}

/// Agent-facing environment filesystem tools; independent of execution grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentToolSurface {
    ReadOnly,
    Edit,
}

/// Prompt loading scope resolved on the selected machine, never on the worker.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentPromptsConfig {
    /// Optional source directories, absolute or relative to the environment
    /// working directory. Explicit nonempty lists replace all defaults,
    /// including home roots. Defaults are .agents/prompts and
    /// .lightspeed/prompts under working directory and execution home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub roots: Option<Vec<String>>,
}

/// Skill discovery scope resolved on the selected machine, never on the worker.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentSkillsConfig {
    /// Optional source directories, absolute or relative to the environment
    /// working directory. Explicit nonempty lists replace all defaults,
    /// including home roots. Defaults are .agents/skills and
    /// .lightspeed/skills under working directory and execution home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub roots: Option<Vec<String>>,
}

/// Grants remote MCP tools by declaring linked servers from the universe MCP
/// catalog; must link at least one server, with unique server ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpFeature {
    #[serde(default = "default_feature_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<McpServerLink>,
}

/// A selected universe MCP server. Its catalog record owns all connection and
/// behavior configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerLink {
    pub server_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartResponse {
    pub session: SessionMutationView,
}

/// Compact acknowledgement returned by session mutations. Call
/// `session/read` when the complete current-state summary is needed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMutationView {
    pub id: SessionId,
    pub status: SessionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_cursor: Option<EventCursor>,
    pub config_revision: u64,
    pub context_revision: u64,
}

/// Replace the session config with a complete document. Anything omitted
/// from the document reverts to defaults; an absent feature is revoked.
/// Requires an idle session; putting an identical document is a no-op that
/// leaves the revision untouched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigPutParams {
    pub session_id: SessionId,
    /// Checked against the session's current config revision when present;
    /// absent replaces unconditionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_config_revision: Option<u64>,
    pub config: SessionConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigPutResponse {
    pub session: SessionMutationView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActiveToolsView {
    pub revision: u64,
    #[serde(default)]
    pub tools: Vec<ToolView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolView {
    pub tool_id: String,
    pub kind: ToolKindView,
    #[serde(default)]
    pub parallelism: ToolParallelismView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ToolKindView {
    /// Code-owned definition; names and schemas are resolved for each turn.
    Builtin { settings: serde_json::Value },
    Function {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description_ref: Option<String>,
        input_schema_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_schema_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_options_ref: Option<String>,
    },
    ProviderNative {
        api_kind: String,
        native_tool_ref: String,
        execution: ProviderNativeToolExecutionView,
    },
    RemoteMcp {
        server_id: String,
        server_label: String,
        server_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_tools: Option<Vec<String>>,
        #[serde(default)]
        approval: RemoteMcpApprovalPolicy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default)]
        auth_required: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ProviderNativeToolExecutionView {
    #[default]
    ProviderHosted,
    ClientEffect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToolParallelismView {
    Exclusive,
    #[default]
    ParallelSafe,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextCompactParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextCompactResponse {
    pub session: SessionMutationView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendParams {
    pub session_id: SessionId,
    pub entries: Vec<ContextAppendEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendEntry {
    /// Stable client-chosen context key. Re-sending the same key with the
    /// same content is a no-op, so the key doubles as the idempotency handle.
    /// `run`, `run.*`, `runtime`, and `runtime.*` are reserved for the runtime.
    pub key: String,
    pub item: InputItem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendResponse {
    pub context_revision: u64,
    pub results: Vec<ContextAppendResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextAppendResult {
    pub key: String,
    pub status: ContextAppendStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<ContextEntryInputView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<InputAdmissionFailureView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_text: Option<String>,
    /// True when `activation_text` was cut off at the server-side length cap.
    /// The committed context entry always holds the full text.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub activation_text_truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ContextAppendStatus {
    Applied,
    Unchanged,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InputAdmissionFailureView {
    pub kind: InputAdmissionFailureKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum InputAdmissionFailureKind {
    UnsupportedMedia,
    UnsupportedAudioMime,
    BlobMissing,
    BlobTooLarge,
    AudioDurationTooLong,
    TranscoderUnavailable,
    TranscodeFailure,
    TranscriptionFailure,
    AdmissionRejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextRemoveParams {
    pub session_id: SessionId,
    /// Active context keys to remove. Removing a key that is already absent
    /// is a per-key no-op (`absent`), so retries are idempotent. Keys under
    /// reserved namespaces (`run`, `run.*`, `runtime`, `runtime.*`) are rejected
    /// request-level.
    pub keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextRemoveResponse {
    pub context_revision: u64,
    pub results: Vec<ContextRemoveResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextRemoveResult {
    pub key: String,
    pub status: ContextRemoveStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<InputAdmissionFailureView>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ContextRemoveStatus {
    Removed,
    Absent,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadParams {
    pub session_id: SessionId,
    /// Newest run summaries to include. Values above the server maximum are
    /// clamped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_limit: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionListParams {
    /// Opaque cursor from the previous page's `nextCursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Only sub-agent sessions whose lineage root is this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<SessionId>,
    /// Only sub-agent sessions delegated directly by this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<SessionId>,
    /// Exclude closed sessions. New sessions that have not run yet remain in
    /// the result alongside open sessions.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exclude_closed: bool,
    /// Only sessions matching every entry (AND semantics). A non-empty value
    /// requires an exact key/value pair; an empty value requires key presence.
    /// Combines with the lineage filters.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionListResponse {
    #[serde(default)]
    pub sessions: Vec<SessionSummaryView>,
    /// Present when more sessions exist past this page. Ordering is most
    /// recently updated first; pages can drift under concurrent activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummaryView {
    pub id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Descriptive key/value metadata; empty when none was set.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    pub lifecycle_status: SessionLifecycleStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at_ms: Option<u64>,
    pub retention: SessionRetentionView,
    /// True only when immutable lifecycle ownership was admitted with a
    /// lifecycle controller at managed-session creation.
    pub managed: bool,
    /// Sub-agent lineage; absent for root sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SessionOriginView>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Effective tree-owned retention for a session. Forks and delegated children
/// name their owning root and project that root's policy and deadline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionRetentionView {
    pub root_session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_after_close_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_at_ms: Option<u64>,
}

/// Typed provenance of a delegated (sub-agent) session: who created it,
/// under which root, at what depth, and the effective limits it was spawned
/// with. Provenance, never ownership — the child is an ordinary session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionOriginView {
    pub kind: SessionOriginKind,
    pub parent_session_id: SessionId,
    pub parent_run_id: RunId,
    /// Nearest ancestor without an origin; root-scoped limits count every
    /// session naming this root.
    pub root_session_id: SessionId,
    /// Absolute depth from the root; a root's direct child is 1.
    pub depth: u32,
    /// The workflow-tool invocation that created the child.
    pub invocation_id: String,
    pub agent: SubagentAgentPin,
    pub limits: SubagentLimitsView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionOriginKind {
    Subagent,
}

/// The profile a sub-agent was spawned from, pinned at its revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubagentAgentPin {
    pub profile_id: ProfileId,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubagentLimitsView {
    pub max_depth: u32,
    pub max_descendants: u32,
    pub max_concurrent: u32,
    pub deadline_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionLifecycleStatus {
    New,
    Open,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionRenameParams {
    pub session_id: SessionId,
    /// New display name; absent clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionRenameResponse {
    pub session: SessionSummaryView,
}

/// Replace a session's metadata with a complete map (the same bounds as
/// `session/start`). Record-only: it does not touch the event log or
/// `updatedAtMs`. There is no merge form; read, edit, and put.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetadataPutParams {
    pub session_id: SessionId,
    /// The complete new map; empty clears it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetadataPutResponse {
    pub session: SessionSummaryView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRetentionPutParams {
    pub session_id: SessionId,
    /// Positive duration enables automatic tree deletion; null disables it.
    #[serde(deserialize_with = "deserialize_required_delete_after_close_ms")]
    #[schemars(
        required,
        schema_with = "required_nullable_delete_after_close_ms_schema"
    )]
    pub delete_after_close_ms: Option<u64>,
}

fn deserialize_required_delete_after_close_ms<'de, D>(
    deserializer: D,
) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<u64>::deserialize(deserializer)
}

fn required_nullable_delete_after_close_ms_schema(
    _: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["integer", "null"],
        "format": "uint64",
        "minimum": 1
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionRetentionPutResponse {
    pub session: SessionSummaryView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleteParams {
    pub session_id: SessionId,
    /// Delete history forks and delegated descendants too. False requires the
    /// target to be a closed retention-tree leaf.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cascade: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleteResponse {
    pub session: SessionSummaryView,
    pub deleted_session_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadResponse {
    pub session: SessionView,
    /// Exclusive upper run-id bound for `session/runs/list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_cursor: Option<RunId>,
    pub has_older_runs: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionEventDirection {
    #[default]
    Forward,
    Backward,
}

impl SessionEventDirection {
    pub fn is_forward(&self) -> bool {
        *self == Self::Forward
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventsReadParams {
    pub session_id: SessionId,
    /// Forward reads follow `after`; backward reads select the latest window
    /// below `before` (or the current head when absent). Both return events
    /// chronologically. Backward reads do not replay reducer state.
    #[serde(default, skip_serializing_if = "SessionEventDirection::is_forward")]
    pub direction: SessionEventDirection,
    /// Exclusive upper sequence bound for backward reads only. Must be positive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<EventCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<EventCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Forward reads only. Long-poll: when no events exist past `after`, hold the request until
    /// one lands or this many milliseconds elapse, then return a normal
    /// (possibly empty) page. Zero or absent preserves immediate return.
    /// Values above the server cap are clamped, not rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventsReadResponse {
    /// Events are always chronological, including backward pages. A page may
    /// begin inside a run or tool batch; clients must retain continuation state.
    #[serde(default)]
    pub events: Vec<SessionEventView>,
    /// Forward: pass as `after`. Backward: pass as `before` to fetch older
    /// history; absent when the beginning is reached. Never use a backward
    /// continuation to advance a forward live cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<EventCursor>,
    /// Backward reads fence the head before reading. The initial backward
    /// page's head is the starting `after` cursor for live forward reads.
    /// An empty backward read returns sequence zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_cursor: Option<EventCursor>,
    /// No more events in the requested direction at the time of this read.
    /// This does not imply that the session is closed.
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<EventLogGap>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloseParams {
    pub session_id: SessionId,
    /// Cancel the active run and drop queued runs instead of rejecting on
    /// active work. Recovers sessions whose workflow no longer exists (e.g.
    /// after an operator terminate) by reconciling the session log directly.
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloseResponse {
    pub session: SessionMutationView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventCursor {
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventLogGap {
    pub requested_after: Option<EventCursor>,
    pub retained_after: Option<EventCursor>,
    pub next_cursor: Option<EventCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventView {
    pub cursor: EventCursor,
    pub session_id: SessionId,
    pub observed_at_ms: u64,
    pub joins: EventJoinsView,
    pub kind: SessionEventKindView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventJoinsView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionEventKindView {
    SessionOpened {
        model: Option<ModelConfig>,
    },
    SessionConfigChanged {
        model: Option<ModelConfig>,
        revision: u64,
    },
    WorkflowToolsConfigured {
        lifecycle_controller_workflow_kind: Option<String>,
        creation_fingerprint: String,
        tool_ids: Vec<String>,
    },
    /// One system-owned workflow-backed tool was durably admitted without
    /// assigning lifecycle ownership to the session.
    SystemWorkflowToolConfigured {
        tool_id: String,
        binding_fingerprint: String,
    },
    WorkflowToolEmitted {
        invocation_id: String,
        tool_id: String,
        semantic_type: String,
        schema_revision: u32,
        binding_fingerprint: String,
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        call_id: String,
        arguments_ref: String,
        /// Keyed completion promises created atomically with the
        /// invocation; absent for notify-only (accepted) invocations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completion_promises: Option<std::collections::BTreeMap<String, String>>,
    },
    WorkflowToolDeliveryFailed {
        invocation_id: String,
        error_ref: String,
    },
    /// Durable start intent for a start-on-call workflow tool. There is no
    /// successful started event; the deterministic execution id makes the
    /// start replay-safe.
    WorkflowToolStartRequested {
        invocation_id: String,
        tool_id: String,
        semantic_type: String,
        schema_revision: u32,
        binding_fingerprint: String,
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        call_id: String,
        arguments_ref: String,
        /// System-derived deterministic execution id of the started plugin
        /// workflow.
        execution_id: String,
        /// Keyed completion promises created atomically with the start
        /// intent.
        completion_promises: std::collections::BTreeMap<String, String>,
    },
    WorkflowToolStartFailed {
        invocation_id: String,
        error_ref: String,
    },
    SessionClosed,
    RunAccepted {
        run_id: RunId,
        submission_id: Option<String>,
        source: RunAcceptedSourceView,
    },
    RunStarted {
        run_id: RunId,
    },
    RunSteeringAccepted {
        run_id: RunId,
        steering_id: String,
        input: Vec<ContextEntryInputView>,
    },
    RunCancellationRequested {
        run_id: RunId,
    },
    ApprovalRequested {
        run_id: RunId,
        approval_id: String,
        subject: ApprovalSubjectView,
    },
    ApprovalRunParked {
        run_id: RunId,
    },
    ApprovalDecided {
        run_id: RunId,
        approval_id: String,
        decision: ApprovalDecisionKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decided_by: Option<PrincipalRefView>,
    },
    ApprovalCancelled {
        run_id: RunId,
        approval_id: String,
    },
    RunCompleted {
        run_id: RunId,
        output: Option<crate::ContentRefView>,
    },
    /// The run ended in failure. `kind` is the engine's classification;
    /// `message` is free text for display.
    RunFailed {
        run_id: RunId,
        kind: RunFailureKindView,
        message: String,
    },
    RunCancelled {
        run_id: RunId,
    },
    PromiseCreated {
        promise_id: String,
        source: String,
    },
    PromiseResolved {
        promise_id: String,
        payload_ref: Option<String>,
    },
    PromiseFailed {
        promise_id: String,
        error_ref: Option<String>,
    },
    PromiseCancelled {
        promise_id: String,
    },
    PromiseDetached {
        promise_id: String,
    },
    TurnStarted {
        run_id: RunId,
        turn_id: String,
    },
    TurnPlanned {
        run_id: RunId,
        turn_id: String,
    },
    TurnGenerationRequested {
        run_id: RunId,
        turn_id: String,
    },
    TurnGenerationCompleted {
        run_id: RunId,
        turn_id: String,
        status: String,
        /// Provider token usage for this generation, including the
        /// prompt-cache read/write counts, when the provider reported it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<LlmUsageView>,
    },
    TurnCompleted {
        turn_id: String,
    },
    /// The run was cancelled while this turn was open; the turn ended without
    /// a generation result.
    TurnCancelled {
        run_id: RunId,
        turn_id: String,
    },
    ContextEntriesApplied {
        base_revision: u64,
        revision: u64,
        entries: Vec<ContextEntryView>,
    },
    ContextEntriesRemoved {
        base_revision: u64,
        revision: u64,
        entry_ids: Vec<ItemId>,
        reason: String,
    },
    ContextKeysRemoved {
        base_revision: u64,
        revision: u64,
        keys: Vec<String>,
    },
    ContextKeyPrefixReplaced {
        base_revision: u64,
        revision: u64,
        key_prefix: String,
        entries: Vec<ContextEntryView>,
    },
    ContextStateReplaced {
        base_revision: u64,
        revision: u64,
        entries: Vec<ContextEntryView>,
        reason: String,
    },
    ContextCompactionRequested {
        base_revision: u64,
        revision: u64,
        trigger: String,
    },
    ContextCompactionFinished {
        base_revision: u64,
        revision: u64,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_ref: Option<String>,
    },
    SkillCatalogSet {
        catalog_ref: Option<String>,
    },
    ToolsReplaced {
        base_revision: u64,
        revision: u64,
    },
    ToolsPatched {
        base_revision: u64,
        revision: u64,
        upserted: Vec<String>,
        removed: Vec<String>,
    },
    ActiveEnvironmentChanged {
        environment_id: Option<EnvironmentId>,
    },
    ToolBatchStarted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        calls: Vec<ToolCallEventView>,
    },
    ToolCallStarted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        call_id: String,
    },
    /// One tool call finished. `output_bytes` is the size of the
    /// model-visible text the tool produced before the runtime's projection
    /// budget was applied, absent for synthetic results; `truncated` is true
    /// when that budget cut it.
    ToolCallCompleted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
        call_id: String,
        status: ToolItemStatus,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        effects: Vec<ToolEffectView>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_bytes: Option<u64>,
        #[serde(default)]
        truncated: bool,
    },
    ToolBatchDeferred {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
    },
    ToolBatchResumed {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
    },
    ToolBatchCompleted {
        run_id: RunId,
        turn_id: String,
        batch_id: String,
    },
}

/// Why a run failed, as the engine classified it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunFailureKindView {
    ModelFailure,
    ToolFailure,
    ContextFailure,
    LimitExceeded,
    Cancelled,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RunAcceptedSourceView {
    Input { entries: Vec<ContextEntryInputView> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEventView {
    pub call_id: String,
    /// Admitted registry identity, absent when the model used an unavailable name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
    pub tool_name: String,
    pub arguments_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<ToolCallDisplayView>,
}

#[cfg(test)]
mod compaction_wire_tests {
    use super::CompactionPolicy;
    use serde_json::json;

    #[test]
    fn editor_compaction_fields_round_trip() {
        for (value, expected) in [
            (
                json!({"mode": "providerTriggered", "compactThresholdTokens": 250000}),
                CompactionPolicy::ProviderTriggered {
                    compact_threshold_tokens: Some(250000),
                },
            ),
            (
                json!({"mode": "providerStandalone", "compactThresholdTokens": 250000, "targetTokens": 100000}),
                CompactionPolicy::ProviderStandalone {
                    compact_threshold_tokens: Some(250000),
                    target_tokens: Some(100000),
                },
            ),
        ] {
            let decoded: CompactionPolicy = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(decoded, expected);
            assert_eq!(serde_json::to_value(decoded).unwrap(), value);
        }
    }

    #[test]
    fn legacy_compaction_fields_remain_readable() {
        for mode in ["providerTriggered", "providerStandalone"] {
            let mut legacy = json!({"mode": mode, "compact_threshold_tokens": 250000});
            let mut canonical = json!({"mode": mode, "compactThresholdTokens": 250000});
            if mode == "providerStandalone" {
                legacy["target_tokens"] = json!(100000);
                canonical["targetTokens"] = json!(100000);
            }
            let decoded: CompactionPolicy = serde_json::from_value(legacy).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), canonical);
        }
    }
}
