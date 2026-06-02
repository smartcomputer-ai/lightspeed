# P63: Target-Scoped Skills

**Status**
- Accepted direction
- Depends on P62 for CAS-backed skill resource trees
- First-cut `engine` skill model types are implemented:
  `SkillState`, skill ids, skill catalog/activation context item kinds,
  and active run/session-scoped activation records.
- First-cut engine command/event/reducer wiring is implemented for setting the
  active skill catalog and replacing active skill activations.
- The engine context planner now inserts the active skill catalog in the stable
  request prefix, inserts direct activation items without top-prepending them,
  avoids parallel context items for tool-call activations, and expires
  run-scoped activations when the run completes.
- OpenAI Responses lowers already-recorded skill context items as explicit
  developer messages, with runtime tests covering catalog and activation
  materialization; API projection exposes minimal skill state events.
- Discovery, catalog rendering, model-selected activation from VFS reads, and
  public API methods are not implemented.
- The first implementation is skill-specific. Do not introduce a generic
  `RuntimeContext` abstraction until there is a second concrete use case.

## Goal

Add skills as a first-class product capability for Forge without turning the
deterministic engine into a filesystem scanner, shell runner, or plugin host.

A skill is a reusable bundle of agent instructions and optional resources. In
Forge, skills should be:

- discoverable from product, user, repo, and host-target sources,
- available from immutable CAS/VFS snapshots or editable VFS workspaces,
- visible through compact skill catalog context items, separate from base
  instructions,
- mounted so referenced docs/scripts/assets are available through ordinary file
  tools, with published skills read-only and authoring roots writable by
  policy,
- activated by loading the relevant `SKILL.md` into context,
- target-aware when installed inside a VM/sandbox,
- replayable because catalog snapshots, mounted resource snapshots, and loaded
  skill content are pinned by CAS refs.

## Context

Modern coding agents use "skills" to package procedural knowledge and local
resources. Codex and Claude Code both implement this pattern around `SKILL.md`
files with progressive disclosure:

```text
metadata in initial context
  -> full SKILL.md loaded only when selected
  -> referenced scripts/references/assets loaded or executed as needed
```

Forge has a different runtime shape. It runs through a deterministic,
event-sourced engine and a Temporal-backed runtime. It may coordinate VM or
sandbox targets through host abstractions, but the core agent should not assume
it owns a Unix process or local filesystem.

Therefore Forge should implement skills as a runtime/catalog/context feature
over CAS and host targets, not as engine-local process state.

## Non-Goals

- Do not execute skill scripts inside `engine`.
- Do not scan local worker filesystems for project skills in hosted mode.
- Do not let a skill grant itself tools or permissions.
- Do not require a Unix environment for instruction-only skills.
- Do not implement Claude's inline shell expansion in `SKILL.md` for v1.
- Do not build a marketplace or plugin distribution system in this roadmap.
- Do not make all skill content permanently visible in every model request.
- Do not implement skill activation approval in the first version. Treat
  discovered skills as valid to activate; future policy can filter discovery or
  add explicit approval gates.
- Do not let activation depend on unpinned mutable host or workspace state;
  every catalog snapshot, tool read, provider materialization, or injected
  skill body must record the exact content refs it used.

## Existing Implementations To Reference

### Codex

Public docs:

- `https://developers.openai.com/codex/skills`

Local implementation checkout:

- `/Users/lukas/dev/tmp/codex`

Important files:

- `codex-rs/core-skills/src/model.rs` - metadata, policy,
  dependencies, load outcome, enabled state.
- `codex-rs/core-skills/src/loader.rs` - scans skill roots, parses
  `SKILL.md` frontmatter, reads optional `agents/openai.yaml`.
- `codex-rs/core-skills/src/render.rs` - renders the model-visible skill list
  with a context budget. Current implementation uses an 8k char default or 2%
  of the context window.
- `codex-rs/core-skills/src/injection.rs` - resolves explicit `$skill`
  mentions and injects selected skill bodies.
- `codex-rs/core-skills/src/invocation_utils.rs` - detects implicit skill
  invocation when scripts or skill docs are read through shell commands.
- `codex-rs/core/src/context/available_skills_instructions.rs` - lowers the
  compact skill catalog as developer-context.
- `codex-rs/core/src/context/skill_instructions.rs` - lowers explicitly loaded
  skill bodies as user-context fragments.
- `codex-rs/core/src/session/turn.rs` - records current-turn user input before
  explicit skill/plugin injection items, so loaded skill bodies are appended near
  the turn tail rather than prepended above existing history.
- `codex-rs/core-skills/src/manager.rs` - skill root resolution, cache, config
  rules, bundled skill install.
- `codex-rs/skills/src/lib.rs` - installs bundled system skills into
  `$CODEX_HOME/skills/.system`.
- `codex-rs/app-server-protocol/schema/typescript/v2/SkillMetadata.ts` and
  related generated files - app-server API skill DTOs.
- `sdk/python/src/openai_codex/_inputs.py` - SDK `SkillInput(name, path)`.

Codex skill shape:

```text
skill-name/
  SKILL.md                  required
  agents/openai.yaml        optional UI/dependency/policy metadata
  scripts/                  optional executable resources
  references/               optional docs loaded on demand
  assets/                   optional templates/images/fonts/etc.
```

Codex discovery uses multiple roots, including repo/project skills, user
skills, bundled system skills, plugin skills, and extra roots. It loads only
name/description/path into the initial prompt. Full skill bodies are injected
after explicit mention or triggering. Scripts and resources are used through
normal filesystem/process tools. A model reading `SKILL.md` through a normal
tool is treated as implicit skill use; the file contents remain the ordinary
tool output in conversation history.

### Claude Code

Public docs:

- `https://code.claude.com/docs/en/skills`
- `https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview`

Local implementation checkout:

- `/Users/lukas/dev/tmp/claude-code`

Important files:

- `src/skills/loadSkillsDir.ts` - core file-based skill loader. It scans
  managed, user, project, additional, and legacy command roots; parses
  frontmatter; creates prompt commands; handles conditional path skills.
- `src/skills/bundledSkills.ts` - bundled skills registry and lazy extraction
  of bundled reference files to disk.
- `src/plugins/builtinPlugins.ts` - exposes enabled built-in plugin skills as
  commands.
- `src/tools/SkillTool/prompt.ts` - Skill tool prompt and bounded skill listing.
- `src/tools/SkillTool/SkillTool.ts` - model-invoked skill execution; inline
  skills return additional model-visible messages plus a short tool result.
- `src/utils/processUserInput/processSlashCommand.tsx` - slash/direct prompt
  command expansion into metadata plus hidden model-visible skill content.
- `src/utils/attachments.ts` and `src/utils/messages.ts` - skill listing and
  discovery attachments rendered as model-visible system reminders.
- `src/services/compact/compact.ts` - preserves invoked skill contents after
  compaction without re-injecting the full skill listing.
- `src/components/skills/SkillsMenu.tsx` - `/skills` UI.
- `src/components/permissions/SkillPermissionRequest/SkillPermissionRequest.tsx`
  - approval UI for skill use.
- `src/utils/plugins/validatePlugin.ts` - plugin skill validation and
  `allowed-tools` frontmatter validation.

Claude Code skill shape:

```text
skill-name/
  SKILL.md
```

It also supports legacy command files. Skill frontmatter supports fields such
as:

- `name`
- `description`
- `when_to_use`
- `allowed-tools`
- `argument-hint`
- `arguments`
- `model`
- `disable-model-invocation`
- `user-invocable`
- `hooks`
- `context: fork`
- `agent`
- `effort`
- `shell`
- `paths`

Claude Code treats skills as prompt commands, often slash-invocable as
`/skill-name`. It substitutes `${CLAUDE_SKILL_DIR}` and
`${CLAUDE_SESSION_ID}`. For file-based skills, it can execute shell expansion
inside prompt markdown so skills can compute dynamic context. MCP skills are
treated as remote/untrusted and do not run inline shell from the markdown body.
The initial model-visible listing is bounded frontmatter/description guidance;
full skill content is loaded only when `SkillTool` or a slash/direct prompt
command invokes the skill. Inline invocation appends model-visible skill content
near the current turn tail. Claude Code separately tracks invoked skills so
compaction can preserve used skill content, while intentionally avoiding
re-injecting the full skill listing after compaction because that is mostly
cache-creation churn.

Claude also supports conditional `paths` skills that are held back until a
matching file is touched.

## Design Position

Forge should support the Agent Skills pattern, but with stricter runtime
boundaries:

- Discovery happens in gateway/worker/runtime services.
- Published skill roots or skill bundles are snapshotted into CAS/VFS and
  mounted read-only before the model is asked to use them.
- Editable skill roots can live in writable VFS workspaces so the model can
  author or revise skills with ordinary file tools.
- The engine records only catalog refs, VFS mount/tool config, active
  activation/read refs, and concrete skill context items that were shown to the
  model.
- Host-installed skills are discovered through the selected host target, not by
  reading the worker's local filesystem.
- The host-target discovery/materialization slices can use traits and
  in-memory/scoped filesystems for tests before real VM/sandbox filesystem and
  process adapters are wired.
- Skill scripts require a real process target and materialized files.

Skills are not a new deterministic engine module in v1. They are a product
feature implemented by runtime services, VFS mounts, ordinary filesystem tools,
and CoreAgent context mechanisms.

## Skill Sources

Forge should support these source categories:

### Product/System Skills

Bundled with Forge or installed by the hosted product.

Storage:

- stored directly as CAS/VFS snapshots,
- published in a runtime catalog,
- available without a host target.

### Organization/User Skills

Configured outside a specific VM, for example in a hosted database or user
settings.

Storage:

- uploaded or synced into CAS/VFS,
- subject to tenant/user policy,
- available without a host target unless scripts require one.

### Repository Skills

Skills stored in the project checkout of a host target.

Candidate roots:

```text
.forge/skills/
.agents/skills/
.codex/skills/
.claude/skills/
```

Support `.forge/skills` as the native Forge location. Support `.agents/skills`
for compatibility with the broader Agent Skills convention. Support Codex and
Claude roots when compatibility mode is enabled.

If the project checkout is mounted as a writable VFS workspace, repository
skills should be discovered from that workspace mount rather than only from an
immutable snapshot. This allows the model to edit existing project skills or
author new ones with ordinary workspace write tools.

### Editable Workspace Skills

Skills can also live in configured writable VFS workspaces, for example:

```text
/workspace/.forge/skills/
/workspace/.agents/skills/
/skills/drafts/
```

This is the authoring path for user- or project-owned skills. A model can
create a new skill directory, edit `SKILL.md`, add references, or revise
scripts/assets through normal VFS write tools when policy allows writes to that
workspace.

Workspace-sourced skills are mutable, so catalog and activation must use
snapshot semantics:

- catalog refresh reads the current workspace head and records the exact refs
  used for skill metadata and catalog context;
- newly authored skills become catalog-selectable only after a catalog refresh;
- activation pins the exact `SKILL.md` contents loaded for the model, even if
  the workspace changes later;
- publishing a workspace-authored skill to system/org/user distribution is a
  separate product workflow, not implicit activation.

### Host-Installed Skills

Skills already installed inside a mounted VM/sandbox, such as:

```text
~/.agents/skills/
~/.codex/skills/
~/.claude/skills/
/etc/forge/skills/
```

These are target-scoped. The same skill path on two VMs is two different skill
sources.

### Plugin/MCP Skills

Defer distribution and install mechanics. The data model should leave room for
plugin and MCP-backed sources, but v1 should not implement a plugin marketplace.

## Native Forge Skill Layout

Forge should accept the common `SKILL.md` layout:

```text
skill-name/
  SKILL.md
  references/
  scripts/
  assets/
```

Required `SKILL.md` frontmatter:

```yaml
---
name: deploy-review
description: Use when reviewing deployment diffs and rollout risk.
---
```

Optional native metadata file:

```text
agents/forge.yaml
```

First-cut fields:

```yaml
interface:
  display_name: Deploy Review
  short_description: Review deployment risk
  default_prompt: Review the current deployment diff.
policy:
  allow_implicit_invocation: true
  trust: project
dependencies:
  tools:
    - type: host
      value: host.run_process
      description: Needed only when running bundled verifier scripts.
resources:
  materialization: on_demand
```

Do not invent a large metadata language in v1. Prefer compatibility with
Codex's `agents/openai.yaml` where the fields overlap.

## Data Model

### Skill Identity

Skill identity must include source and target scope.

```rust
pub struct SkillId(String);

pub enum SkillSource {
    Cas {
        snapshot_ref: BlobRef,
        skill_path: VfsPath,
    },
    Workspace {
        workspace_id: VfsWorkspaceId,
        root_path: VfsPath,
        skill_path: VfsPath,
    },
    HostPath {
        target: ToolExecutionTarget,
        root_path: String,
        skill_path: String,
    },
    Plugin {
        plugin_id: String,
        skill_name: String,
    },
    Remote {
        source_id: String,
        skill_name: String,
    },
}
```

Recommended ID forms:

```text
skill:cas:<snapshot-digest>:<path-digest>
skill:workspace:<workspace-id>:<path-digest>
skill:host:<target-namespace>:<target-id>:<path-digest>
skill:plugin:<plugin-id>:<skill-name>
```

Human-readable names are not stable identities. Duplicate names are allowed
across targets and sources.

### Skill Metadata

```rust
pub struct SkillMetadata {
    pub skill_id: SkillId,
    pub name: String,
    pub description: String,
    pub short_description: Option<String>,
    pub source: SkillSource,
    pub scope: SkillScope,
    pub target: Option<ToolExecutionTarget>,
    pub enabled: bool,
    pub trust: SkillTrustLevel,
    pub interface: Option<SkillInterface>,
    pub dependencies: SkillDependencies,
    pub location: SkillLocation,
    pub skill_doc_ref: Option<BlobRef>,
}

pub enum SkillLocation {
    MountedSnapshot {
        source_snapshot_ref: BlobRef,
        source_mount_path: VfsPath,
        skill_dir_path: VfsPath,
        skill_doc_path: VfsPath,
    },
    MountedWorkspace {
        workspace_id: VfsWorkspaceId,
        workspace_head_ref: BlobRef,
        source_mount_path: VfsPath,
        skill_dir_path: VfsPath,
        skill_doc_path: VfsPath,
    },
    Remote {
        source_id: String,
        skill_name: String,
    },
}
```

`MountedSnapshot.source_snapshot_ref` points to the P62 VFS snapshot for the
skill source root or bundle. It does not have to be a one-skill snapshot. A
single mounted snapshot can contain many skill directories.

`MountedWorkspace.workspace_id` and `workspace_head_ref` identify the writable
workspace and the head snapshot observed when the catalog entry was built.

`source_mount_path`, `skill_dir_path`, and `skill_doc_path` are paths inside
the mounted VFS view, for example `/skills/system/openai-docs` and
`/skills/system/openai-docs/SKILL.md`, or
`/workspace/.forge/skills/deploy-review/SKILL.md`.

`skill_doc_ref` points at the `SKILL.md` body or full markdown payload.
It is recorded when the catalog builder or activation path has read and pinned
that exact file.

For future remote skills whose instructions and resources arrive through
different channels, add separate resource refs then. Do not force that
complexity into the v1 local/CAS/VFS path.

### Skill Catalog Snapshot

The runtime should build a per-session or per-run catalog snapshot:

```rust
pub struct SkillCatalogSnapshot {
    pub schema_version: String, // "forge.skills.catalog.v1"
    pub target: Option<ToolExecutionTarget>,
    pub skills: Vec<SkillMetadata>,
    pub warnings: Vec<SkillLoadWarning>,
}
```

Store the catalog snapshot in CAS. Do not pre-render provider-specific catalog
messages in the engine. The core context item should point at the semantic
catalog blob and let provider adapters materialize it into provider-native
input messages or content blocks.

```rust
pub struct SkillCatalogContext {
    pub catalog_ref: BlobRef,
}
```

The final provider request blob remains the audit record for the exact
provider-native text/messages sent to the model. If catalog budgeting later
requires a narrowed or selected catalog, store that selected semantic catalog
as another catalog blob rather than storing provider-specific rendered text in
core state.

### Skill State And Context Items

Forge needs a skill context lane that is neither `instructions_ref` nor normal
user history. Keep this lane skill-specific in v1 instead of abstracting it as
generic runtime context.

`SessionConfig.context.instructions_ref` remains the base instruction/system
prompt mechanism. It should not contain the skills catalog. Session config can
later hold skill policy and source configuration, but it should not be the home
for active catalog snapshots or loaded skill bodies.

The core split is:

- `CoreAgentState.skills` records current skill planning state: active catalog
  plus skill activations currently eligible for request planning in the active
  run.
- `ContextItemKind::SkillCatalog` and `ContextItemKind::SkillActivation` record
  semantic skill context selected for a request. The final provider request
  records the exact provider-native materialization.
- Provider adapters lower these concrete skill context items with
  provider-native semantics. OpenAI Responses should render them as developer
  messages first.

First-cut engine shape:

```rust
pub struct CoreAgentState {
    pub context: ContextState,
    pub skills: SkillState,
    // ...
}

pub struct SkillState {
    pub catalog: Option<SkillCatalogContext>,
    pub activations: Vec<SkillActivation>,
}

pub enum ContextItemKind {
    Message { role: ContextMessageRole },
    SkillCatalog,
    SkillActivation { skill_id: SkillId },
    ToolCall { call_id: ToolCallId, name: ToolName },
    ToolResult { call_id: ToolCallId, is_error: bool },
    // ...
}
```

This avoids a premature `RuntimeContextKind` / `RuntimeContextAuthority` /
`RuntimeContextLifecycle` abstraction while preserving the real contract:

- skill context items are part of the request context, not conversation
  history;
- skill catalog items are stable request-prefix context;
- loaded skill bodies are current-run tail context or ordinary tool results,
  never prepended above existing history;
- skill catalog items are not summarized during compaction;
- the active catalog can be reinserted from pinned refs after compaction;
- activated skill bodies are reinserted only while they remain present in
  `SkillState.activations`;
- provider adapters render skill context with provider-native semantics.

For provider APIs without a separable skill-guidance lane, the adapter must use
an explicit configured fallback or fail clearly for skills-enabled sessions. Do
not silently fold the catalog into `instructions_ref`.

### Catalog Lifecycle And Context Injection

Skill headers are read during runtime catalog discovery, not inside `engine`.

Discovery reads each candidate `SKILL.md` enough to parse YAML frontmatter
(`name`, `description`, and optional short description) and reads optional
metadata such as `agents/forge.yaml` or compatible `agents/openai.yaml`.
For CAS/VFS snapshot sources this should happen through VFS snapshot reads and
the blob store. For writable workspace sources it should happen through the VFS
workspace mount at a recorded workspace head. For host-target sources it should
happen through the selected host filesystem abstraction, followed by
snapshotting the source root into CAS/VFS.

Recommended refresh points:

- session open, after skills config and initial VFS mounts are known;
- explicit `skills/list` or catalog refresh with `force_refresh`;
- skills config changes;
- host-target catalog refresh when a target is added or a user asks to refresh.

Do not rescan mutable host files during each model turn. A run should use the
active catalog snapshot and mounted source refs it was prepared with.
Writable VFS workspaces are session state, not external host state, but they
are still mutable. Workspace-authored skill changes should become catalog
metadata only through explicit catalog refresh or another product-controlled
refresh boundary.

The catalog context selected for the model is skill context, not real user
history and not base instructions:

1. Build the active `SkillCatalogSnapshot`.
2. Store the catalog snapshot as `catalog_ref`.
3. Record the active catalog in `CoreAgentState.skills.catalog`.
4. When request planning includes the catalog, record a
   `ContextItemKind::SkillCatalog` item whose `native_item_ref` points at
   `catalog_ref`.
5. Insert that semantic skill catalog item in the stable request prefix, before
   the conversation window. Catalog updates may invalidate the prompt cache, so
   make refresh explicit and relatively rare.
6. Let provider adapters lower the semantic catalog into provider-native
   model input.

Record the source `catalog_ref` so `session/read` can explain which skills
were visible. Do not represent the catalog as a normal user message, and do
not append it to
`SessionConfig.context.instructions_ref`.

Explicit skill activation is different from the compact catalog. A selected
skill's `SKILL.md` body may be injected as a separate
`ContextItemKind::SkillActivation` block or returned through the ordinary
`read_file` tool result.

### Compaction And Re-Injection

Skill catalog context should behave like canonical skill state, not transcript
content.

Pre-turn or manual compaction should compact only the conversation/tool history
that is eligible for summarization. It should not ask the compaction model to
preserve the skills catalog. The next model turn rebuilds the request from
`CoreAgentState.skills.catalog` plus the compacted conversation state,
so the current skills catalog is reinserted from the active `catalog_ref`.

Mid-turn compaction should rebuild skill catalog context from canonical state
rather than trusting the compaction output to carry provider-lowered skill
context forward. The catalog returns to the stable request prefix; live direct
activations return near the current-run tail if still active.

This keeps skill catalog visibility independent from summaries. If the active
catalog changes, future runs or refreshed turns use the new catalog snapshot;
already-recorded provider requests still pin the exact provider-native request
blob they used.

Activated skill bodies are different from the catalog. Do not automatically
reinsert every previously activated skill after compaction. Reinsert an
activated body only while its `SkillActivation` remains in
`SkillState.activations`.

When reinserting, deduplicate by the pinned `context_ref`. If the
original tool result or explicit `SkillActivation` context item with that same
`context_ref` is still present in the planned request window, do not add
a second copy. Once the activation is removed from `SkillState.activations`,
it is just ordinary history: compaction may summarize or omit it, and the
model can read the cataloged `SKILL.md` again if the skill becomes relevant
later.

### Skill Activation

```rust
pub struct SkillActivation {
    pub skill_id: SkillId,
    pub catalog_ref: BlobRef,
    pub context_ref: BlobRef,
    pub source: SkillActivationSource,
    pub scope: SkillActivationScope,
}
```

Activation pins the selected catalog snapshot through `catalog_ref` and the
loaded provider-neutral skill context through `context_ref`. The context blob
may be raw semantic skill text for v1, or a richer structured payload later.
Provider adapters wrap that payload in the appropriate provider-native message
or content block. Activation does not make the skill folder appear. Enabled
skill roots should already be available through VFS mounts before the model can
use them: read-only mounts for published sources, or writable workspace mounts
for authoring sources.

Source/load provenance belongs to the active catalog, tool result, context
item, and optional projection/report data. Do not duplicate that provenance in
`SkillActivation` unless request planning needs it.

`source` records only the direct activation source needed by engine/projection:
`ToolCall { call_id }` for activations derived from tool execution, or
`Direct` for activations admitted by runtime/API/UI flow. Inspect the tool call
to distinguish ordinary `read_file` from an explicit activation helper.
`scope` controls context maintenance: `Run` activations are removed when the
current run completes; `Session` activations remain active across runs until
explicit deactivation, policy removal, or session close.

Do not add a separate activation id in v1. The active list is live
request-planning state, not a durable activation ledger. Use `skill_id` for
the selected skill, `context_ref` for the exact loaded skill context,
`ToolCallId` for model-selected reads, and `ContextItemId` for historical
inclusions.

An activation in `SkillState.activations` is live request-planning state for
the current session/run. It should be removed when it is no longer active.
Historical evidence that a skill body was injected or read lives in context
items and tool results in the event log; deactivation must not delete those
historical items or mutate provider requests that already included the body.

If a model reads a cataloged `skill_doc_path` through the ordinary VFS
`read_file` tool, the runtime may emit this activation record from that tool
call. If a user explicitly selects a skill through UI/CLI, the runtime may read
the same `skill_doc_path` before the model turn and inject the loaded
`SKILL.md` as `ContextItemKind::SkillActivation`. In both cases, the
activation remains in `SkillState.activations` only while it is active for its
configured scope.

Multiple skills may be active at the same time. Activation is additive, not a
global mode switch. If two active skill bodies conflict, normal instruction
priority, trust level, and recency rules apply; the runtime should surface the
ambiguity in projection rather than silently picking one.

## Target Scoping

Forge already has `ToolExecutionTarget`:

```rust
pub struct ToolExecutionTarget {
    pub namespace: String,
    pub id: String,
}
```

Use this as the target identity for host-installed skills.

Examples:

```text
host:local
host:vm-123
host:sandbox-456
```

Skill discovery and activation must be target-aware:

```text
discover skills for host:vm-123
  -> read configured roots through host:vm-123 filesystem
  -> snapshot discovered skill roots into CAS/VFS
  -> mount snapshots read-only under /skills/<source-id>
  -> catalog entries carry target = host:vm-123
```

A model-visible skill list should show target scope when ambiguity matters:

```text
- deploy-review (target host:vm-123) - Review deployment diffs.
- deploy-review (global) - Review hosted deploy manifests.
```

The current core default target machinery is useful for explicit activation
helpers and future materialization requests. If an optional
`forge.skill.activate` helper is added, it can use
`ToolTargetRequirement::Optional { namespace: "host" }` so the active default
host target is attached to the tool call when present.

For explicit non-default target activation, the activation arguments should
also accept a target id. If model-selected per-call execution targets become a
common need beyond skills, extend the core tool-call target model later instead
of adding skill-specific routing hacks.

## Skill Root Mounts

Mount skill roots or skill bundles, not only individual skill directories.
The mount source can be an immutable snapshot or a writable workspace.

Examples:

```text
/skills/system/
  openai-docs/SKILL.md
  skill-creator/SKILL.md
  imagegen/SKILL.md

/skills/repo-main/
  deploy-review/SKILL.md
  release-notes/SKILL.md

/workspace/.forge/skills/
  draft-skill/SKILL.md
  draft-skill/references/example.md
```

A catalog entry points at one skill directory inside a mounted root:

```text
source_snapshot_ref = sha256:...
source_mount_path   = /skills/system
skill_dir_path      = /skills/system/openai-docs
skill_doc_path      = /skills/system/openai-docs/SKILL.md
```

For a workspace-authored skill, the catalog entry points into the writable
workspace mount and records the workspace head observed during refresh:

```text
workspace_id       = vfsws_...
workspace_head_ref = sha256:...
source_mount_path  = /workspace
skill_dir_path     = /workspace/.forge/skills/draft-skill
skill_doc_path     = /workspace/.forge/skills/draft-skill/SKILL.md
```

Prefer one snapshot/mount per source root or product-managed bundle. Fall back
to one snapshot/mount per skill only when policy isolation, source shape, or
size limits require it.

Current P62 VFS mounts are explicit session records. A snapshot ref is not a
model-visible path until it is mounted. P62 also rejects nested mounts, so do
not mount `/skills` and then child mounts under `/skills/...`. Mount concrete
source roots such as `/skills/system` and `/skills/repo-main`; the mounted VFS
adapter can synthesize parent directories for listing.

Published/system/org/user skill mounts should be read-only. Editable skill
roots should live under writable workspace mounts, for example
`/workspace/.forge/skills` or a dedicated writable `/skills/drafts` mount.
Mutating tools must still fail on read-only skill mounts.

## Discovery

Discovery is a runtime operation. It should never run in `engine`.

Inputs:

- session config,
- tenant/user policy,
- known host targets,
- target capabilities,
- root configuration,
- size/depth limits,
- compatibility modes.

Output:

- `SkillCatalogSnapshot` CAS ref,
- warnings for invalid/unreadable skills,
- optional projected API skill list.

Discovery steps:

1. Resolve skill roots for the requested target or global source.
2. Snapshot host/product source roots into P62 VFS when allowed, or select the
   configured VFS workspace mount for editable roots.
3. Mount immutable snapshots read-only at stable session paths; keep workspace
   roots under their writable workspace mounts.
4. List candidate skill directories inside those mounted snapshot or workspace
   roots.
5. Read `SKILL.md` frontmatter.
6. Validate name, description, policy, dependencies, and size limits.
7. Store metadata, resolved `SkillLocation`, pinned `SKILL.md` refs when read,
   and warnings in a catalog snapshot.
8. Render a compact catalog for model context.

For host targets, all filesystem reads must go through the host abstraction.

## Progressive Disclosure

Initial skill catalog context should include only compact metadata:

```text
## Skills
Available skills:
- openai-docs: Use when ... Path: /skills/system/openai-docs/SKILL.md
- deploy-review [host:vm-123]: Use when ... Path: /skills/vm-123/deploy-review/SKILL.md

When a skill is relevant, read its `SKILL.md` before following its workflow.
```

Do not inject all `SKILL.md` bodies by default.

Use a hard budget. Codex currently uses an 8k char default or 2% of the context
window; Forge can start with the same rule.

If the catalog exceeds budget:

- keep all names visible if possible,
- truncate descriptions before omitting skills,
- emit a warning event or projection field,
- prefer target-local and explicitly configured skills over broad global
  catalogs.

## Activation

Activation is the act of loading a selected skill's `SKILL.md` into the model
context and recording the pinned content refs. It is not the act of mounting
the skill folder. Enabled skills should already be available through mounted
snapshot or workspace roots as part of catalog/session preparation.

There are two activation paths:

1. Model-selected activation: the model reads the cataloged `skill_doc_path`
   through the ordinary VFS `read_file` tool. The tool result contains the
   `SKILL.md` contents. The runtime can recognize that path as a cataloged
   skill doc and record a `SkillActivation`.
2. Explicit user activation: UI/CLI selection such as `$deploy-review` resolves
   a skill by id or unambiguous name before the model turn. The runtime reads
   that same `skill_doc_path` and injects a skill context item directly, saving
   a tool round.

Resolution rules for explicit activation:

- Prefer `skill_id` when provided.
- If only `name` is provided, it must be unambiguous within the active catalog
  and target scope.
- If a host target is required, use the explicit target argument or session
  default target.
- Resolve to the cataloged `skill_doc_path`; do not rescan mutable host files
  during activation.
- If the skill comes from a writable VFS workspace, read through the workspace
  mount at the request's planned workspace head and pin the exact body that was
  loaded. If the workspace changed since catalog refresh, projection should
  report that the activation body came from a newer workspace head than the
  catalog metadata. Explicit name/id activation may require a refresh when
  policy wants catalog metadata and body to match exactly.
- Materialization is separate from activation. Only materialize resources into
  a real host target when scripts/assets need a process-visible path.

Model-visible result:

```xml
<skill>
<name>deploy-review</name>
<id>skill:host:host:vm-123:...</id>
<target>host:vm-123</target>
<path>/skills/vm-123/deploy-review/SKILL.md</path>
... contents of SKILL.md ...
</skill>
```

The exact wrapper can be provider-specific, but the content should be a normal
context item or tool result recorded in the session log.

Current VFS `read_file` data:

- successful tool calls already produce `ToolCallResult.output_ref`,
  `model_visible_output_ref`, and generic `effects`;
- `output_ref` is structured JSON for `ReadFileResult`, including requested
  path, resolved path, selected text, line start/count, total lines, truncation,
  and bytes read;
- `model_visible_output_ref` is the model-facing text returned from the tool;
- VFS effects currently record workspace commits from mutating tools, not file
  read provenance.

Therefore model-selected skill activation can initially key off:

1. successful `read_file`,
2. `ReadFileResult.resolved_path` matching a cataloged `skill_doc_path`,
3. a complete read of the file (`line_start == 1` and `truncated == false`).

If the read is partial, treat it as an ordinary file read, or record a partial
read observation, but do not claim the full skill body was activated. To record
the exact workspace head used by a workspace-backed `read_file`, add a narrow
VFS read-provenance effect such as `forge.vfs.read_file.v1` containing
`workspace_id`, `workspace_head_ref`, `mount_path`, and resolved path. Snapshot
reads can similarly include `snapshot_ref` when useful. The activation record
should reuse that tool result/effect data instead of inventing a separate
parallel read log. When a full `SKILL.md` read is recognized, derive
`context_ref` from the loaded `ReadFileResult.text` bytes or from a
provider-neutral loaded skill context blob built by the runtime.

`forge.skill.activate` is optional in v1. It is useful as an API/runtime helper
for explicit UI activation or resolving by name, and could later host approval
workflows if product policy needs them. It should not be required for
model-selected skills. The normal path for model-selected skills is
`read_file` on the mounted `SKILL.md`.

## Activation Lifetime

Do not model skills as a single active mode. A run can have multiple active
skill activations.

Distinguish three concepts:

- The skill catalog is compact model guidance listing available skills and
  mounted `SKILL.md` paths.
- `SkillActivation` in `SkillState.activations` is live request-planning state:
  the loaded skill body is currently eligible for inclusion under its run or
  session scope.
- `ContextItemKind::SkillActivation` is the historical record that an exact
  loaded skill body was shown to the model.
- The request context window is the subset actually included in one provider
  request after budget planning.

Active activations remain eligible through model/tool turns, including
compaction, while their scope is active. Run-scoped activations are removed
when the run completes. Session-scoped activations remain across runs until
explicit deactivation, policy removal, or session close. Removing a live
activation does not remove historical context items, tool results, or provider
requests.

Context pressure is not the same as deactivation. If active skill bodies exceed
the request budget, the context planner may omit lower-priority active skills
from a particular request and record an inclusion report or warning. It should
not silently mark them deactivated. Priority should prefer user-pinned skills,
explicit user-selected skills, recently activated skills, and higher-trust
skills. A model can reload an omitted skill by reading its cataloged
`SKILL.md` again.

The planner should avoid duplicating a skill body. If the same
`context_ref` is already present as a tool result or
`SkillActivation` item in the planned request window, the active activation is
satisfied for that request and no additional skill block should be inserted.

Prompt-cache ordering matters. Request planning must not insert new skill
activations at the top of the context window, because that changes the stable
prefix and invalidates cached history. Keep base instructions, tool schemas, the
active catalog, and existing conversation/tool history in their normal order.
When a direct activation has no prior tool result, append its semantic
`SkillActivation` item near the current-run tail. When a tool call loaded the
skill, the ordinary tool result is the loaded skill body and no extra activation
item is needed.

Add an explicit deactivate path when clients need it:

```text
skills/deactivate
```

Deactivation stops future sticky reinsertion. It does not delete historical
activation records, tool results, explicit skill context items, or provider
requests that already included the skill.

## Engine Integration

Keep v1 minimal, but make the engine model skill-native:

- Add `SkillState` to `CoreAgentState` for active catalog and active
  run/session-scoped activations.
- Add concrete `ContextItemKind::SkillCatalog` and
  `ContextItemKind::SkillActivation` variants for model-visible skill context
  that was actually included in a request.
- Keep active catalog snapshots and active activations out of `ContextConfig`;
  session config can later hold skill policy and source configuration.
- Teach context-window planning and provider request materialization to include
  `SkillCatalog` context in the stable request prefix and direct
  `SkillActivation` context near the current-run tail.
- Use existing VFS mount/tool configuration so the model can read mounted
  skill files with ordinary filesystem tools.
- Use existing tool result flow when the model reads a `SKILL.md`.
- Store explicit activation outputs and `read_file` results as CAS blobs like
  any other tool result.
- Keep historical activation evidence in context/tool events. Projection can
  derive model-selected activation from cataloged `SKILL.md` reads until a
  stronger typed event is needed.

First-cut model types:

```rust
pub struct SkillState {
    pub catalog: Option<SkillCatalogContext>,
    pub activations: Vec<SkillActivation>,
}

pub enum ContextItemKind {
    Message { role: ContextMessageRole },
    SkillCatalog,
    SkillActivation { skill_id: SkillId },
    ToolCall { call_id: ToolCallId, name: ToolName },
    ToolResult { call_id: ToolCallId, is_error: bool },
    // ...
}
```

First-cut command/event wiring:

```rust
CoreAgentCommand::SetSkillCatalog {
    catalog: Option<SkillCatalogContext>,
}

CoreAgentCommand::SetSkillActivations {
    activations: Vec<SkillActivation>,
}

pub enum SkillEvent {
    CatalogSet {
        catalog: Option<SkillCatalogContext>,
    },
    ActivationsSet {
        activations: Vec<SkillActivation>,
    },
}
```

`SetSkillActivations` replaces the active set. The engine rejects duplicate
active `skill_id`s for now. The context planner, not external command
admission, owns inserting skill context items into the request context.
Skill catalog and activation commands are admitted only while no run is active
or queued. Direct activation therefore does not start work; it updates
`SkillState`, and the next requested run consumes that state during context
planning. Deactivation can stop future sticky reinsertion by replacing the
live activation set without rewriting history.

For activations sourced from a tool call, the loaded `SKILL.md` is already
visible through the ordinary tool result context item. The planner should treat
that tool result as satisfying the activation for the current request/window
and avoid inserting a duplicate `SkillActivation` item. For direct
activations, there is no prior tool result, so the planner inserts a
`ContextItemKind::SkillActivation { skill_id }` item using
`SkillActivation.context_ref` near the current-run tail, not above existing
history or above the stable catalog prefix.

Do not add commands such as `ScanSkills` or `ReadSkillFile` to the engine.
Do not add a special engine command for model-selected skill activation when a
normal VFS file read already expresses the behavior.

## Public API

Add product-shaped APIs only where clients need them.

Candidate methods:

```text
skills/list
skills/activate
skills/deactivate
session/skills/list
session/skills/configure
```

Recommended v1:

- `skills/list` for UI/CLI discovery before or during a session.
- `session/read` projection includes active skill catalog summary, active skill
  activations, and historical skill context items.
- Activation during model execution uses ordinary VFS `read_file` on the
  cataloged `SKILL.md` path.
- Manual user activation can be encoded as run input or a future
  `skills/activate` method that records a `SkillActivation` and optional
  `SkillActivation` context item.

`skills/list` request shape:

```rust
pub struct SkillsListParams {
    pub session_id: Option<SessionId>,
    pub target: Option<ToolExecutionTarget>,
    pub force_refresh: bool,
}
```

Response shape:

```rust
pub struct SkillsListResponse {
    pub catalog_ref: BlobRef,
    pub skills: Vec<SkillSummary>,
    pub warnings: Vec<SkillLoadWarning>,
}
```

## Configuration

Session config should eventually include:

```rust
pub struct SkillsConfig {
    pub enabled: bool,
    pub include_system: bool,
    pub compatibility: SkillCompatibilityConfig,
    pub roots: Vec<SkillRootConfig>,
    pub workspace_roots: Vec<SkillWorkspaceRootConfig>,
    pub disabled: Vec<SkillSelector>,
    pub allow_implicit_selection: bool,
    pub allow_workspace_authoring: bool,
    pub activation_policy: SkillActivationPolicy,
    pub max_active_skills: Option<u32>,
}
```

Workspace roots are configured VFS workspace paths that may contain skills and
may be writable:

```rust
pub struct SkillWorkspaceRootConfig {
    pub workspace_id: VfsWorkspaceId,
    pub root_path: VfsPath,
    pub writable: bool,
    pub auto_catalog_refresh: bool,
}
```

Default `auto_catalog_refresh` to false. Explicit refresh keeps catalog changes
under user/product control and avoids treating every workspace write as a
prompt-surface change.

Compatibility config:

```rust
pub struct SkillCompatibilityConfig {
    pub forge: bool,
    pub agents: bool,
    pub codex: bool,
    pub claude: bool,
}
```

Native Forge roots should be on by default for Forge sessions. Codex/Claude
compatibility roots should be opt-in or product-configured to avoid surprising
tool behavior.

## Scripts And Unix Requirements

Instruction-only skills require only CAS/VFS reads.

Reference-only skills require:

- VFS read/list/search tools over the mounted skill root.

Script-backed skills require:

- process capability on the selected host target,
- materialized skill resources visible to that process when the process cannot
  read directly from the VFS adapter,
- an interpreter such as `bash`, `python3`, or `node` if the script depends on
  one.

Forge should make this explicit when a selected skill has scripts but the
current target cannot run them:

```text
This skill has scripts but target host:vm-123 has no process capability.
Loaded instructions only; script execution unavailable.
```

Do not execute shell snippets embedded inside `SKILL.md` by default. If Forge
later supports dynamic skill rendering, represent it as an explicit trusted
renderer step with policy, target, timeout, and recorded output.

## Permissions And Trust

Skills declare dependencies; they do not grant permissions.

Rules:

- `allowed-tools` or `dependencies.tools` is a requested capability set.
- The session/user/tenant policy decides what is actually available.
- Project and host-installed skills are less trusted than system/user skills,
  but v1 does not implement activation approval. Discovery/configuration is the
  policy boundary: if a skill is discovered into the active catalog, assume it
  is valid to activate.
- Scripts require separate policy grants.
- Remote/MCP skills must not run embedded shell renderers.

Trust levels:

```rust
pub enum SkillTrustLevel {
    System,
    Organization,
    User,
    Project,
    Host,
    Remote,
}
```

Default posture:

- system/org/user skills can be implicitly suggested,
- project/host skills can be listed and activated once discovered,
- remote skills require explicit install/configuration before discovery.

## Mutable Catalog Refresh

Host-installed skills and workspace-authored skills may change while a session
is running.

Use snapshot semantics:

- catalog refresh can discover new metadata,
- catalog refresh writes a new skill-root snapshot and updates mount/catalog
  paths for future runs when the source is an external host path,
- catalog refresh over a writable workspace records the workspace head snapshot
  and makes newly authored or edited skills catalog-selectable,
- reading or explicitly activating `SKILL.md` pins that exact file content into
  CAS,
- existing activations do not change when source files change,
- session replay uses the pinned activation refs.

If a host target emits filesystem-change notifications, the gateway can emit a
`skills/changed` notification and refresh the target catalog. Do not require
watching for v1; explicit refresh is enough. For workspace sources, ordinary
VFS write tool effects already expose new workspace revisions; the catalog does
not need to refresh automatically after every write.

## Interaction With P62 VFS

P63 should use P62 like this:

```text
Published skill source root or bundle
  -> P62 snapshot_ref
  -> read-only session mount at /skills/<source-id>
  -> SkillMetadata.location = MountedSnapshot {
       source_snapshot_ref,
       source_mount_path,
       skill_dir_path,
       skill_doc_path
     }
  -> model reads /skills/<source-id>/<skill>/SKILL.md with VFS tools
  -> optional materialization into host target only for scripts/assets

Editable skill source root
  -> writable VFS workspace mount, such as /workspace
  -> SkillMetadata.location = MountedWorkspace {
       workspace_id,
       workspace_head_ref at catalog refresh,
       source_mount_path,
       skill_dir_path,
       skill_doc_path
     }
  -> model edits /workspace/.forge/skills/<skill>/... with VFS tools
  -> catalog refresh makes new/edited skills selectable
  -> activation reads and pins exact SKILL.md body from the workspace
```

The model should be able to read skill references through VFS tools without
knowing whether the skill originated in CAS, a database, or a VM.

Do not assume a snapshot ref is itself a model-visible path. It becomes
file-like only through a VFS mount. A workspace ref is also not a plain path;
it becomes file-like through its writable mount. Prefer multi-skill mounts at
source-root, bundle, or workspace-root granularity; use one-skill mounts only
for isolation or source-shape reasons.

## Implementation Slices

The phases are ordered from prerequisite/core behavior to broader product
surface. G0-G4 are the essential first usable layer. G5-G9 can be added as the
product needs them; they should not force complexity into the initial engine
model.

### G0: Skill Core Model Prerequisite

Essential.

- Add skill-native core types: skill ids, `SkillState`, `SkillCatalogContext`,
  `SkillActivation`, and concrete skill context item kinds.
- Keep active catalog snapshots and active activations out of `ContextConfig`;
  reserve session config for future skill policy/source configuration.
- Keep skill catalog and activation context separate from `instructions_ref`
  and normal transcript history.
- Teach context-window planning to place `SkillCatalog` before conversation
  history.
- Implement OpenAI Responses lowering first, materializing the skills catalog
  as a developer message.
- Ensure compaction reinserts canonical skill catalog context from pinned refs
  rather than relying on summaries to preserve it.
- This does not require P71 prompt bundle editing, prompt workspaces, or the
  full prompt-management UI.

### G1: Skill Model And Parser

- Add skill metadata structs outside `engine`.
- Use discriminated location/source structs rather than many optional fields.
- Parse `SKILL.md` YAML frontmatter.
- Parse optional `agents/forge.yaml`.
- Accept compatible Codex `agents/openai.yaml` fields where straightforward.
- Add validation tests for names, descriptions, malformed YAML, and missing
  fields.

### G2: Read-Only CAS/VFS Skill Roots

Essential.

- Snapshot skill roots or bundles into P62 VFS.
- Support multiple skills inside one snapshot/mount.
- Mount enabled skill roots read-only at stable session paths.
- Store root snapshot refs, mount paths, skill directory paths, and `SKILL.md`
  refs.
- Add size/depth/file-count limits.
- Add tests for scripts/references/assets trees and multiple skills in one
  mounted root.

### G3: Global Catalog

- Load product/system and configured user/org skills from CAS/VFS.
- Build a semantic catalog snapshot with budget/selection metadata where
  needed.
- Store catalog snapshots in CAS.
- Record the catalog ref in `SkillState.catalog` before a run starts.
- Reinsert the configured catalog context after compaction from the pinned
  catalog ref, not through the compaction summary.

### G4: Model-Selected Activation Through VFS Reads

Essential.

- Treat ordinary VFS `read_file` calls against cataloged `SKILL.md` paths as
  model-selected activation.
- Reuse current tool result data: `output_ref`, `model_visible_output_ref`, and
  parsed `ReadFileResult.resolved_path`.
- Count the read as full activation only when it starts at line 1 and is not
  truncated.
- Record active activation metadata with skill id, catalog ref, loaded
  provider-neutral context ref, source, and run/session scope.
- Add a narrow VFS read-provenance effect if needed to record exact
  workspace-head or snapshot provenance for the read.
- Add model-selected activations to `SkillState.activations` when the loaded
  skill body should remain eligible for planning.
- Add tests that activation/read pins skill content even if the source changes
  after the catalog snapshot.

### G5: Explicit User Activation And Deactivation

Useful, but can follow the core path.

- For explicit UI/CLI selection, resolve by skill id or unambiguous name and
  pre-read the cataloged `SKILL.md`.
- Return or inject a model-visible skill block for explicit activation.
- Add explicit activations to `SkillState.activations` for their configured
  scope.
- Support multiple active skill activations in one run.
- Reinsert activated bodies after compaction only while they remain in
  `SkillState.activations`, and do not duplicate the body when the original
  `context_ref` is already in the request window.
- Add `skills/deactivate` to remove active activations when clients need it.

### G6: Writable Workspace Skill Authoring

Needs more product validation.

- Support configured writable VFS workspace roots for skill authoring.
- Discover configured workspace skill roots from writable VFS mounts when
  authoring is enabled.
- Store workspace ids, mount paths, skill paths, and observed head refs in the
  catalog.
- For workspace-sourced activations, record the workspace id/head observed by
  the read or runtime injection that loaded the body.
- Add tests that authored or edited workspace skills become catalog-visible
  only after catalog refresh.

### G7: Host Target Discovery

Broader target-scoped layer.

- Discover skills through a selected `ToolExecutionTarget`.
- Support `.forge/skills` and `.agents/skills` first.
- Add Codex/Claude compatibility roots behind config.
- Snapshot host skill roots into CAS and mount the snapshots read-only.
- Add tests with in-memory/scoped host filesystems.
- Wire real VM/sandbox host filesystem discovery when host-target filesystem
  adapters are available.

### G8: Materialization For Scripts

Needs more policy and target work.

- Integrate P62 materialization.
- Include materialized root path when script execution requests it and policy
  allows it.
- Validate process capability and interpreter availability where practical.
- Add tests for no-process target, read-only target, and materialization
  warnings.
- Wire real VM/sandbox materialization when host-target filesystem/process
  adapters are available.

### G9: API And Projection

Product surface.

- Add `skills/list` if needed by CLI/UI.
- Project active catalog refs and activated skills through `session/read`.
- Emit warnings for invalid skills and catalog truncation.

## Verification

Core tests for G0-G4:

- parse valid Forge skill,
- reject invalid frontmatter,
- tolerate optional metadata read failures with warnings,
- enforce size/depth limits,
- snapshot and mount a skill root containing multiple skills,
- build catalog with duplicate names across targets,
- build catalog within budget,
- record catalog context in `SkillState.catalog` rather than
  `instructions_ref` or real user input,
- record actual request inclusion as `ContextItemKind::SkillCatalog`,
- materialize the skills catalog as an OpenAI Responses developer message,
- reinsert configured catalog context after compaction without relying on the
  compaction summary,
- expose cataloged `skill_doc_path` values under read-only VFS mounts,
- treat `read_file` of a cataloged `SKILL.md` as activation,
- do not treat partial/truncated `SKILL.md` reads as full activation,
- record activation using existing tool result refs and resolved path,
- add/read VFS provenance effect when exact read-time snapshot or workspace
  head cannot be recovered from existing data,
- activation survives later source file mutation.

Expanded-phase tests:

- resolve explicit `skill_id`,
- reject ambiguous name activation,
- explicit UI/CLI activation pre-reads the same `SKILL.md`,
- reinsert an activated skill body after compaction only while it remains in
  `SkillState.activations`,
- avoid duplicating an activated skill body when the original tool result or
  `SkillActivation` item with the same `context_ref` is still in the
  request window,
- allow multiple active skill activations in one run,
- remove active activations when the run completes without deleting history,
  tool results, or provider requests,
- remove active activations on explicit deactivation or config/policy removal,
- allow removed activations to remain only as historical context/tool records
  without sticky reinsertion,
- discover skills under a configured writable VFS workspace root,
- expose cataloged `skill_doc_path` values under writable workspace mounts when
  authoring is enabled,
- create a new workspace skill with VFS write tools and make it
  catalog-selectable after explicit refresh,
- edit a workspace skill and verify existing catalog/activation refs remain
  pinned until refresh or reactivation,
- materialize the skills catalog as an Anthropic Messages skill-guidance user
  message or other explicit configured fallback when that adapter is built,
- scripts are unavailable without process capability,
- materialized script paths point at target-local roots.

## Open Questions

- Should Forge support Claude `paths` conditional activation in v1?
  Recommendation: not initially. Add after target file-change signals exist.
- Should Forge execute shell expansions in `SKILL.md`?
  Recommendation: no for v1. Add only as an explicit trusted renderer.
- Should native Forge metadata live in `agents/forge.yaml` or `forge.yaml`?
  Recommendation: `agents/forge.yaml` to stay parallel with
  `agents/openai.yaml`.
- Should Codex and Claude compatibility roots be enabled by default?
  Recommendation: enable `.agents/skills`; make `.codex/skills` and
  `.claude/skills` explicit compatibility modes.
- Should activation be a tool or an API command?
  Recommendation: neither is required for model-driven activation. Use
  ordinary VFS file reads for model-selected skills. Add an API/runtime helper
  only for UI/CLI explicit activation or name resolution.
