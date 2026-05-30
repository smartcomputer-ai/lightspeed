# P63: Target-Scoped Skills

**Status**
- Accepted direction
- Depends on P62 for CAS-backed skill resource trees
- Not implemented

## Goal

Add skills as a first-class product capability for Forge without turning the
deterministic engine into a filesystem scanner, shell runner, or plugin host.

A skill is a reusable bundle of agent instructions and optional resources. In
Forge, skills should be:

- discoverable from product, user, repo, and host-target sources,
- synced or snapshotted into CAS/VFS,
- visible through compact catalog context,
- activated explicitly or by model request,
- target-aware when installed inside a VM/sandbox,
- replayable because activated skill content is pinned by CAS refs.

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
normal filesystem/process tools.

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

Claude also supports conditional `paths` skills that are held back until a
matching file is touched.

## Design Position

Forge should support the Agent Skills pattern, but with stricter runtime
boundaries:

- Discovery happens in gateway/worker/runtime services.
- Skill directories are snapshotted into CAS/VFS.
- The engine records only catalog refs, activation refs, context items, and
  tool config.
- Host-installed skills are discovered through the selected host target, not by
  reading the worker's local filesystem.
- Skill scripts require a real process target and materialized files.

Skills are not a new deterministic engine module in v1. They are a product
feature implemented by runtime services and ordinary CoreAgent context/tool
mechanisms.

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
skill:cas:<snapshot-digest>
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
    pub snapshot_ref: Option<BlobRef>,
    pub skill_doc_ref: Option<BlobRef>,
    pub resource_root_ref: Option<BlobRef>,
}
```

`snapshot_ref` points to the P62 VFS snapshot for the skill directory when the
skill has been synced to CAS.

`skill_doc_ref` points at the `SKILL.md` body or full markdown payload.

`resource_root_ref` is normally the same as `snapshot_ref`, but leaving it
separate allows future remote skills whose instructions and resources arrive
through different channels.

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

Store the catalog snapshot in CAS. The rendered model-visible skill list should
also be stored in CAS when it becomes part of a run context.

### Skill Activation

```rust
pub struct SkillActivation {
    pub skill_id: SkillId,
    pub name: String,
    pub target: Option<ToolExecutionTarget>,
    pub activation_reason: SkillActivationReason,
    pub arguments: Option<String>,
    pub skill_doc_ref: BlobRef,
    pub resource_root_ref: Option<BlobRef>,
    pub materialized_root: Option<MaterializedSkillRoot>,
}
```

Activation pins the skill content. If the source is a host path, activation
must snapshot the skill into CAS before injecting it into context.

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
  -> snapshot discovered skill directories into CAS/VFS
  -> catalog entries carry target = host:vm-123
```

A model-visible skill list should show target scope when ambiguity matters:

```text
- deploy-review (target host:vm-123) - Review deployment diffs.
- deploy-review (global) - Review hosted deploy manifests.
```

The current core default target machinery is useful for this. A skill
activation tool can use `ToolTargetRequirement::Optional { namespace: "host" }`
so the active default host target is attached to the tool call when present.

For explicit non-default target activation, the activation arguments should
also accept a target id. If model-selected per-call execution targets become a
common need beyond skills, extend the core tool-call target model later instead
of adding skill-specific routing hacks.

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
2. List candidate directories.
3. Read `SKILL.md` frontmatter.
4. Validate name, description, policy, dependencies, and size limits.
5. Snapshot the skill directory into P62 VFS when allowed.
6. Store metadata and warnings in a catalog snapshot.
7. Render a compact catalog for model context.

For host targets, all filesystem reads must go through the host abstraction.

## Progressive Disclosure

Initial model context should include only compact metadata:

```text
## Skills
Available skills:
- openai-docs: Use when ...
- deploy-review [host:vm-123]: Use when ...

Use forge.skill.activate to load a skill before following its workflow.
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

First-cut activation should be a tool/runtime effect:

```text
forge.skill.activate
```

Input schema:

```json
{
  "type": "object",
  "properties": {
    "skill_id": { "type": "string" },
    "name": { "type": "string" },
    "target": {
      "type": "object",
      "properties": {
        "namespace": { "type": "string" },
        "id": { "type": "string" }
      }
    },
    "arguments": { "type": "string" },
    "materialize": { "type": "boolean" }
  }
}
```

Resolution rules:

- Prefer `skill_id` when provided.
- If only `name` is provided, it must be unambiguous within the active catalog
  and target scope.
- If a host target is required, use the call execution target, explicit target
  argument, or session default target.
- If the source is a host path, snapshot it into CAS before activation.
- If `materialize = true`, materialize the P62 VFS snapshot into the selected
  host target and include the materialized root path in the activation result.

Model-visible result:

```xml
<skill>
<name>deploy-review</name>
<id>skill:host:host:vm-123:...</id>
<target>host:vm-123</target>
<path>/skills/deploy-review/SKILL.md</path>
... contents of SKILL.md ...
</skill>
```

The exact wrapper can be provider-specific, but the content should be a normal
context item recorded in the session log.

## Engine Integration

Keep v1 minimal:

- Use `CoreAgentCommand::SetToolRegistry` to expose `forge.skill.activate`.
- Use existing tool result flow to return activated skill instructions.
- Store activation outputs as CAS blobs like any other tool result.

Recommended near-term engine improvement:

```rust
CoreAgentCommand::RecordRuntimeContextItems {
    items: Vec<UncommittedContextItem>,
}
```

This would let the runtime admit activated skill content as context without
pretending the activation is just an ordinary tool result. It also helps API
projection and compaction.

First implementation can use:

```rust
ContextItemSource::Runtime { label: "skill_activation".to_string() }
provider_kind = Some("forge.skill.activation.v1".to_string())
```

Later, if projection needs stronger typing, add:

```rust
ContextItemKind::Skill {
    skill_id: SkillId,
    target: Option<ToolExecutionTarget>,
}
```

Do not add commands such as `ScanSkills` or `ReadSkillFile` to the engine.

## Public API

Add product-shaped APIs only where clients need them.

Candidate methods:

```text
skills/list
skills/activate
session/skills/list
session/skills/configure
```

Recommended v1:

- `skills/list` for UI/CLI discovery before or during a session.
- `session/read` projection includes active skill catalog summary and activated
  skills.
- Activation during model execution uses the tool path.
- Manual user activation can be encoded as run input or a future
  `skills/activate` method that records runtime context.

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
    pub disabled: Vec<SkillSelector>,
    pub allow_implicit_activation: bool,
    pub activation_policy: SkillActivationPolicy,
}
```

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

- VFS read/list/search tools, or
- host filesystem reads if the skill lives only on a host target.

Script-backed skills require:

- process capability on the selected host target,
- materialized skill resources visible to that process,
- an interpreter such as `bash`, `python3`, or `node` if the script depends on
  one.

Forge should make this explicit in activation:

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
- Project and host-installed skills are untrusted unless policy says otherwise.
- Activating an untrusted skill may require approval.
- Scripts require separate approval or policy grants.
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
- project/host skills can be listed but may require approval before activation,
- remote skills require explicit install/approval.

## Target-Scoped Catalog Refresh

Host-installed skills may change while a session is running.

Use snapshot semantics:

- catalog refresh can discover new metadata,
- activation pins current content into CAS,
- existing activations do not change when source files change,
- session replay uses the pinned activation refs.

If a host target emits filesystem-change notifications, the gateway can emit a
`skills/changed` notification and refresh the target catalog. Do not require
watching for v1; explicit refresh is enough.

## Interaction With P62 VFS

P63 should use P62 like this:

```text
Skill source directory
  -> P62 snapshot_ref
  -> SkillMetadata.snapshot_ref
  -> model-visible virtual path /skills/<id>/SKILL.md
  -> optional materialization into host target for scripts/assets
```

The model should be able to read skill references through VFS tools without
knowing whether the skill originated in CAS, a database, or a VM.

## Implementation Slices

### G1: Skill Model And Parser

- Add skill metadata structs outside `engine`.
- Parse `SKILL.md` YAML frontmatter.
- Parse optional `agents/forge.yaml`.
- Accept compatible Codex `agents/openai.yaml` fields where straightforward.
- Add validation tests for names, descriptions, malformed YAML, and missing
  fields.

### G2: CAS/VFS Skill Snapshot

- Snapshot skill directories into P62 VFS.
- Store `SKILL.md` body and root snapshot refs.
- Add size/depth/file-count limits.
- Add tests for scripts/references/assets trees.

### G3: Global Catalog

- Load product/system and configured user/org skills from CAS/VFS.
- Render a compact model-visible catalog with budget enforcement.
- Store catalog snapshots in CAS.

### G4: Host Target Discovery

- Discover skills through a selected `ToolExecutionTarget`.
- Support `.forge/skills` and `.agents/skills` first.
- Add Codex/Claude compatibility roots behind config.
- Snapshot host skills into CAS.
- Add tests with in-memory/scoped host filesystems.

### G5: Activation Tool

- Register `forge.skill.activate`.
- Resolve by skill id or unambiguous name.
- Return a model-visible skill block.
- Record activation output as a context item/tool result.
- Add tests that activation pins host skill content even if source files change
  afterward.

### G6: Materialization For Scripts

- Integrate P62 materialization.
- Include materialized root path in activation when requested and allowed.
- Validate process capability and interpreter availability where practical.
- Add tests for no-process target, read-only target, and materialization
  warnings.

### G7: API And Projection

- Add `skills/list` if needed by CLI/UI.
- Project active catalogs and activated skills through `session/read`.
- Emit warnings for invalid skills and catalog truncation.

## Verification

Required tests:

- parse valid Forge skill,
- reject invalid frontmatter,
- tolerate optional metadata read failures with warnings,
- enforce size/depth limits,
- build catalog with duplicate names across targets,
- render catalog within budget,
- resolve explicit `skill_id`,
- reject ambiguous name activation,
- snapshot host skill before activation,
- activation survives later host file mutation,
- scripts are unavailable without process capability,
- materialized script paths point at target-local roots,
- untrusted skill activation follows policy.

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
  Recommendation: tool for model-driven activation; API method later for UI
  and manual user activation.
