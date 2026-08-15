# P113: Explicit VFS And Environment Tool Domains

**Status**

- Completed 2026-08-02.
- Tranche 1 complete: the deterministic engine/API target model has been
  removed, environment calls are no longer made unavailable by the engine
  when no environment is selected, and batch requests retain the bounded
  `active_environment_id` fact.
- Tranche 2 complete in the runtime model: built-ins now have explicit VFS or
  environment domains, stable `vfs.*`/`env.*` logical ids, provider adapter
  identity separate from logical routing, and direct VFS/environment runtime
  contexts. The generic target registry and fused hosted filesystem path are
  no longer used.
- Tranche 3 complete: the catalog is VFS-owned (`skills.catalog.vfs`, catalog
  id/source `vfs`), activation state retains catalog identity, prompt and
  instruction discovery remain VFS-only, and legacy aliases/fused-router code
  have been removed.
- Tranche 4 complete: generated contracts and TypeScript consumers are
  current, the full offline verification matrix is green, current architecture
  documentation has been updated, and the legacy-target/fused-filesystem
  removal audit is clean.
- Greenfield breaking refactor. Do not preserve compatibility aliases, dual
  routing, legacy target fields, or the fused session-filesystem behavior.
- Supersedes the fused-filesystem decisions in
  [P75](p75-environment-ready-tools-refactor.md),
  [P76](p76-environment-runtime-projection.md), and the routing section of
  [P108](p108-universe-environments.md). It preserves their useful generic
  `FileSystem` implementations, explicit active-environment state, live
  environment capability resolution, and VFS workspace-link topology.
- No workspace materialization, checkout, check-in, or synchronization is part
  of this milestone.

## Implementation Result

P113 is implemented end to end. The shipped shape is:

- `vfs.*` bindings and provider-visible `vfs_*`/`Vfs*` tools operate only on
  resolved session workspace links;
- `env.*` file, process, and new-job bindings operate only on the active
  environment captured on the tool batch;
- provider adapter identity is separate from the stable logical binding id;
- the generic execution-target model and runtime target registry are gone;
- the fused session filesystem and its host/fused route metadata are gone;
- environment filesystem adapters preserve the environment-native path and cwd
  contract while applying the environment record's read-only restriction;
- prompt/instruction discovery remains VFS-only; and
- the skill catalog is explicitly identified as `skills.catalog.vfs`, with
  catalog identity retained on activation context for future independent
  catalogs.

Offline verification completed on 2026-08-02:

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test -p engine -p tools -p llm-runtime -p temporal-workflow -p temporal-server -p api --no-fail-fast
cargo test -p eval
cargo run -p api --bin export-schema
cd clients/typescript && npm run typecheck && npm run test && npm run build
cd ../configurator-mcp && npm run check
git diff --check
```

Relevant live verification was subsequently completed against the running
local Temporal/PostgreSQL/host-bridge stack:

```bash
source scripts/dev/env.sh
cargo test -p temporal-server --test environment_provider_live -- --ignored --test-threads=1 --nocapture
cargo test -p temporal-server --test temporal_live temporal_live_session_start_then_run_start_completes_fake_runs -- --ignored --test-threads=1 --nocapture
```

The environment-provider suite includes a focused P113 case that writes and
reads `skills/SKILL.md` through the active environment while reading different
bytes at that same path through `vfs_read_file`. All five environment-provider
tests and the focused session/config reconciliation test passed. Other opt-in
live suites outside the P113 filesystem scope were not run.

The prompt-level eval harness was also extended with independent VFS and active
environment roots, Anthropic Messages support, provider-specific case
applicability, same-path read/edit isolation cases, and absolute environment
cwd regression cases. Live provider results were green:

```bash
cargo run -p eval -- --provider openai all     # 12/12 passed
cargo run -p eval -- --provider anthropic all  # 11/11 passed, apply-patch skipped
```

The Anthropic Claude-like surface includes `VfsListDir` and the ordinary
environment `ListDir`, both backed by the same domain-isolated canonical
listing operation. This keeps directory inspection available without requiring
an environment shell and covers the absolute `/workspaces` path regression on
both providers.

## Summary

VFS and environment filesystems are different products with different
consistency models. Give each a distinct model-visible tool family and never
combine them into one path namespace:

```text
vfs_* tools
  -> session-linked VFS snapshots and workspaces

ordinary file tools + process tools + new job starts
  -> the session's currently active environment
```

Remove the generic tool execution-target system. The session's
`active_environment_id` is already explicit deterministic state and is already
copied onto each tool-batch activity request. Environment actions consume that
bounded fact directly; VFS and all other tools dispatch from their admitted
runtime binding.

The current skill catalog becomes explicitly VFS-owned. Prompt/instruction
sourcing remains VFS-only. A future environment skill catalog is a separate
catalog associated with one environment, not another root merged into the VFS
catalog.

The central invariants are:

> A tool name always identifies one filesystem domain.

> Selecting an environment never changes the meaning of a VFS tool, and VFS
> configuration never grants or restricts environment file access.

> VFS files are not implied to be visible to environment processes.

## Problem

The current hosted runtime exposes ordinary file tools only when
`features.vfs.tools` is granted, even though an active environment may expose a
complete filesystem. It then executes those tools against `fs:session`, a
runtime-composed router containing:

1. the linked VFS workspace routes; and
2. the active environment filesystem at `/`.

VFS routes win collisions. Process tools see only the environment filesystem.
This makes the same path mean different bytes to file and process tools:

```text
VFS link:               /workspace
environment directory:  /workspace

read_file("/workspace/src/lib.rs")
  -> VFS

exec_command("cat /workspace/src/lib.rs")
  -> environment
```

Writes made through file tools may be invisible to compilation and tests.
Writes made by a process may be hidden from later file reads. Relative paths
are especially misleading because the fused file context prefers the active
environment cwd even when a VFS route shadows that cwd.

The fusion also creates secondary coupling:

- `features.vfs.tools` controls whether environment files can be read or
  edited;
- a VFS read-only surface can hide write tools for a writable environment;
- deriving edit tools from environments would allow those tools to reach VFS
  paths unless another policy layer were added;
- directory listings synthesize a namespace that does not exist inside the
  environment;
- `fs:session` does not identify the concrete environment involved in a call;
- environment-only file failures still mention missing VFS mounts; and
- projection types retain unused host/fused route and
  `same_state_as_active_env` concepts.

Making the fusion correct would require materialization ownership, dirty-state
tracking, checkpoints, shared-workspace conflict handling, and synchronization
semantics comparable to a version-control system. Those features may be useful
later, but ordinary file access must not depend on them.

## Decision 1: Separate Tool Families

### Environment tools

Ordinary file tools are environment actions. They operate on the filesystem of
the environment selected in deterministic session state:

```text
read_file
write_file
edit_file
apply_patch
grep
glob
list_dir
exec_command
write_stdin
job_submit
job_run
job_read
```

`features.environments` installs the ordinary filesystem and basic process
surface. `features.environments.jobs` continues to grant the advanced job
surface. Tool visibility represents potential authority; the selected
environment's live capabilities determine whether a particular operation can
execute:

```text
filesystem_read
filesystem_write
process_start
process_stdin
job_start / job_read / ...
```

If no environment is active, an environment action returns the structured
runtime failure `no_active_environment`. If the active environment lacks the
required capability, it returns a capability-specific failure. The engine does
not pre-classify the call as unavailable.

Environment discovery and selection tools remain independent:

- `environment_read` may use an explicit id or the active id;
- `environment_list`, `environment_activate`, and `environment_deactivate`
  remain gated by `selectionTools`; and
- selecting an environment does not mutate VFS links, catalogs, prompts, or
  instructions.

### VFS tools

`features.vfs.tools` installs a dedicated VFS surface over
`features.vfs.workspaceLinks`.

The read-only surface is:

```text
vfs_read_file
vfs_grep
vfs_glob
vfs_list_dir
```

The edit surface adds:

```text
vfs_write_file
vfs_edit_file
vfs_apply_patch
```

These operations reuse the generic filesystem implementations and the existing
`LinkedVfsFileSystem`. Snapshot links remain immutable. Workspace mutations
continue to create CAS snapshots and compare-and-set the shared workspace head
against its revision. Concurrent sessions therefore receive an explicit
revision conflict rather than silently overwriting one another.

Do not add VFS copy, move, generic remove, checkout, check-in, materialization,
or synchronization tools in P113. `vfs_write_file`, `vfs_edit_file`, and
`vfs_apply_patch` match the current ordinary editing surface, including the
existing patch-based creation/deletion behavior.

### Stable logical ids

Use provider-independent runtime logical ids:

```text
vfs.read_file
vfs.write_file
vfs.edit_file
vfs.apply_patch
vfs.grep
vfs.glob
vfs.list_dir

env.read_file
env.write_file
env.edit_file
env.apply_patch
env.grep
env.glob
env.list_dir
env.run_process
env.write_process_stdin
env.job_submit
env.job_run
env.job_read
```

P113 is greenfield. Remove `fs.*`, `host.*`, and other compatibility aliases
instead of retaining several names for the same operation.

## Decision 2: Preserve Provider-Native Tool Shapes

The separation is semantic; it must not flatten provider-specific tool
contracts.

Provider-default ordinary environment tools retain their familiar surface:

- OpenAI Responses and OpenAI Completions use the canonical OpenAI-oriented
  names and argument schemas;
- Anthropic Messages uses the Claude-Code-like names and argument schemas; and
- an explicit Codex-like presentation continues to use its Codex-oriented
  schemas.

VFS tools use distinct names so they cannot collide with the ordinary tools,
while mirroring the corresponding provider's argument and result shape.
Conceptually:

| Operation | OpenAI/canonical | Anthropic/Claude-like |
| --- | --- | --- |
| read | `vfs_read_file` | `VfsRead` |
| write | `vfs_write_file` | `VfsWrite` |
| edit | `vfs_edit_file` | `VfsEdit` |
| patch | `vfs_apply_patch` | `VfsApplyPatch` |
| grep | `vfs_grep` | `VfsGrep` |
| glob | `vfs_glob` | `VfsGlob` |
| list | `vfs_list_dir` | `VfsListDir` |

The exact provider-visible descriptions must state the storage boundary:

- VFS tools access only linked virtual workspaces/snapshots;
- ordinary file and command tools access only the active environment; and
- neither family implies that its files exist in the other domain.

The provider-facing name is presentation. Runtime dispatch always uses the
stable logical id.

## Decision 3: Remove Generic Execution Targets

Delete the generic `ToolExecutionTarget` and `ToolTargetRequirement` model,
including `None`, `SessionFilesystem`, `ActiveEnvironment`, and `Fixed` target
requirements.

Do not replace it with a reduced `ToolExecutionRequirement`. The runtime
already knows an operation's domain from its admitted binding, and the workflow
already supplies the selected environment as bounded batch context.

The target-free execution shape is:

```text
CoreAgentState.environment.active_environment_id
  -> ToolInvocationBatchRequest.active_environment_id
  -> environment executor
```

The activity request is recorded by Temporal. Retries therefore consume the
same environment id even if the session later selects another environment.
The event log also records the environment-selection event before the tool
batch, so replay reconstructs which environment was selected at that sequence.
A per-call string namespace/id duplicates both facts.

Remove `execution_target` from:

- observed/planned invocation policy;
- `ToolCallStarted` and related tool events/state;
- `ToolInvocationRequest`;
- validation helpers and codecs;
- API projections and display helpers; and
- test fixtures.

Retain `active_environment_id` on `ToolInvocationBatchRequest`. Environment
policy remains beside it. VFS workspace links remain separate bounded batch
facts.

### Runtime dispatch

Runtime dispatch follows the trusted tool binding/logical operation:

```text
vfs.*                 -> VFS context
env.read/edit/...     -> active environment filesystem
env.run_process       -> active environment process executor
env.job_submit/run    -> active environment job executor
env.job_read          -> environments named by job handles
web.*, mcp.*, fleet.* -> their existing executors
workflow tools        -> their admitted workflow bindings
```

The generic `ToolTargets` registry and namespace validation disappear. A
runtime may hold semantic contexts directly:

```text
SessionToolRuntime
  vfs?
  active_environment?
  web / fleet / workflow dependencies

EnvironmentToolContext
  environment_id
  filesystem?
  process?
  jobs?
```

It is correct for one real environment context to expose both filesystem and
process capabilities. The bad coupling was representing VFS as a fake host or
environment, not grouping capabilities that genuinely belong to one
environment.

### Job routing

New work uses the active environment captured for the originating batch:

- `job_submit` and `job_run` pin that environment in their workflow-owned
  execution context; and
- later session selection cannot redirect the submitted job.

Handle-based operations are intentionally different. `job_read`, cancellation,
and future handle operations resolve the environment id embedded in each job
handle. They never reroute through the current selection.

### Selection batches

An environment activate/deactivate call cannot share a batch with an operation
whose meaning consumes the active environment:

- ordinary environment file tools;
- process/stdin tools; or
- new job starts.

VFS, web, and other environment-independent calls do not semantically depend on
selection and may share a batch. Runtime batch validation derives this from the
trusted binding, not from a target namespace.

## Decision 4: Delete The Fused Filesystem

Remove the hosted fused routing path completely:

- do not mount an active environment filesystem under a session VFS;
- do not give VFS routes precedence over environment paths;
- do not synthesize mixed directory listings;
- do not share or fall back between VFS and environment cwd values; and
- do not silently choose one backend when the other is unavailable.

Keep the generic `FileSystem`, scoped/read-only adapters, local/remote host
implementations, VFS snapshot/workspace implementations, and reusable file
operation handlers.

Delete `SessionFileSystem` when no remaining legitimate non-fused consumer
exists. Remove or narrow all associated types:

```text
SessionFileSystemRoute
SessionFileSystemRouteMetadata
SessionFileSystemRouteSource::EnvironmentFilesystem
same_state_as_active_env
FsRouteSource::HostFilesystem
FsRouteSource::FusedWorkspace
```

An environment's remote filesystem adapter lives directly on
`EnvironmentToolContext`. The linked VFS filesystem is built only for VFS tool
calls and VFS prompt/skill projection.

Environment-only batches must not resolve workspace links or require a VFS
workspace store. VFS-only batches must not resolve or connect to an active
environment.

Use domain-specific failures:

```text
no_active_environment
environment_filesystem_unavailable
environment_filesystem_read_only
no_vfs_workspace_links
vfs_workspace_revision_conflict
```

## Decision 5: VFS-Owned Prompts And Instructions

Prompt/instruction sourcing remains exclusively VFS-owned for the foreseeable
future:

```text
features.vfs.prompts
  -> linked VFS roots
  -> CAS-backed instruction context
```

Environment selection and environment filesystem contents never participate in
automatic prompt/instruction discovery. P113 does not define an environment
prompt source or a generic filesystem prompt scanner.

Keep the current pre-run refresh behavior: resolve linked VFS heads, snapshot
selected content into CAS, and reconcile instruction context before admitting a
new run. VFS edits made by any session therefore continue to update sourced
instructions at the existing refresh boundary.

Instruction context entries should retain explicit VFS provenance. Remove any
wording that describes them as coming from a generic session filesystem.

## Decision 6: A VFS-Specific Skill Catalog

The existing automatically refreshed skill catalog is the VFS skill catalog,
not a generic catalog assembled from every filesystem visible to a session.

P113 makes that identity explicit:

```text
context key: skills.catalog.vfs
catalog id:  vfs
source kind: vfs
```

Keep `ContextEntryKind::SkillCatalog` generic so future independent catalogs
can share provider rendering and context-retention behavior. Distinguish
catalogs by key and serialized catalog identity rather than creating a context
kind for every source.

The VFS catalog builder accepts only:

- linked snapshot roots; and
- linked workspace roots with their observed head snapshot.

Remove host/environment variants from the VFS catalog path, including generic
target fields and `HostRoot`, `EnvironmentPath`, and `HostFilesystem` locations. The
shared frontmatter parser and source-neutral skill metadata fields may remain
reusable.

The serialized catalog envelope should identify its source explicitly:

```text
SkillCatalogSnapshot
  schema_version
  catalog_id = "vfs"
  source = { type: "vfs" }
  skills
  warnings
```

Provider prompt text calls it the "VFS skill catalog" and instructs the model
to read a selected `SKILL.md` through the appropriate VFS file tool. VFS skill
locations must never imply that `exec_command` can address the same path.

Automatic VFS skill refresh remains unchanged in substance:

```text
features.vfs.skills
  -> resolve configured VFS roots and current workspace heads
  -> fingerprint roots
  -> rebuild/reuse the CAS catalog snapshot
  -> reconcile skills.catalog.vfs before a run
```

### Future environment skill catalogs

Skills installed inside a real environment are a plausible future capability,
but they will use a separate catalog, for example:

```text
context key: skills.catalog.environment.<environment-id>
catalog id:  environment:<environment-id>
source:
  type: environment
  environment_id: <environment-id>
```

Such a catalog may appear only while its environment is selected or according
to a later explicit policy. It has environment-local locations and directs the
model to ordinary environment file/process tools. It is not merged into the
VFS source fingerprint, and environment files never alter
`skills.catalog.vfs`.

P113 does not implement environment skill discovery. The VFS catalog identity
and activation metadata should avoid assuming that skill ids are globally
unique across all future catalogs; catalog identity must remain available when
resolving activation and presenting provenance.

## Configuration Semantics

The existing sparse feature document remains the authority:

```text
features.vfs
  workspaceLinks
  tools?       // none | readOnly | edit
  prompts?
  skills?

features.environments
  providers?
  selectionTools
  jobs
```

Do not add `features.filesystem` or an environment filesystem sub-grant in
P113. Presence of `features.environments` grants potential use of the selected
environment's filesystem/process surface; live provider capabilities enforce
availability and writability.

The effective standard toolset is derived as follows:

| Feature | Derived tools |
| --- | --- |
| no feature | model only |
| `vfs.tools = readOnly` | read-only `vfs_*` surface |
| `vfs.tools = edit` | complete `vfs_*` surface |
| `environments` | ordinary environment file/process surface plus `environment_read` |
| `environments.selectionTools` | list/activate/deactivate additions |
| `environments.jobs` | environment job bindings/read surface and their concurrency dependencies |

When both features are present, both file families are present. Their paths
may contain unrelated bytes without collision or precedence because no shared
router exists.

## Projection And Model Framing

The VFS catalog remains a projection of only `features.vfs.workspaceLinks`.
Rename its model framing from a generic "Filesystem" description to an
explicit VFS description:

```text
Virtual filesystem (VFS):
  <linked routes>

Use vfs_* tools for these paths. VFS files are not visible to environment file
tools or commands. Ordinary file and command tools operate only on the active
environment.
```

`environment_read` remains the live source of environment identity, cwd,
status, and capabilities. Do not project an environment root into the VFS
catalog.

Tool-call presentation can still use operation-aware displays, but it should
distinguish VFS and environment actions. For example, `vfs_read_file` is a VFS
read while `read_file`/`Read` is an environment read.

## API, State, And Compatibility

P113 is an internal and model-tool contract break. The public session config
shape can remain unchanged, but generated schemas and references must be
regenerated if removing target fields changes public views.

There is no migration path for active greenfield sessions:

- reconcile/remove old ordinary file tool specs derived from VFS;
- install new `vfs.*` and `env.*` specs from config;
- remove target-bearing tool state during the breaking state-model change;
- reset local development state when persisted event/schema compatibility
  requires it; and
- do not retain legacy aliases for provider-visible or logical tool names.

Update the current architecture documents when implementing P113:

- `README.md` capability and VFS examples;
- `docs/design.md` tool/runtime description;
- `docs/spec/04-environments.md` routing section;
- `AGENTS.md` architecture rules; and
- any profile/config examples that imply ordinary file tools operate on VFS.

Historical roadmap documents remain historical, but their status/header should
point readers to P113 where their fused routing decisions are no longer
current.

## Implementation Plan

### 1. Introduce explicit VFS built-ins

- Add VFS logical operations and bindings for the seven-tool surface.
- Reuse canonical, Codex-like, and Claude-Code-like schemas and invocation
  adapters while assigning collision-free VFS names.
- Split `BuiltinToolsetConfig` into semantic VFS and environment filesystem
  surfaces instead of one generic `fs` block.
- Add toolset tests for provider-visible names, descriptions, schemas,
  documents, parallelism, and logical ids.

### 2. Move ordinary file tools under environments

- Derive ordinary file/process tools whenever `features.environments` is
  present.
- Put the remote filesystem adapter on `EnvironmentToolContext` beside process
  and job capabilities.
- Dispatch ordinary filesystem operations through the active environment from
  the batch request.
- Return structured no-selection and capability failures.

### 3. Remove targets from engine and runtime contracts

- Delete generic target domain types, target requirements, validators, and
  constants.
- Remove target fields from tool specs, call state/events, invocation DTOs,
  codecs, projections, fixtures, and tests.
- Delete `ToolTargets`/`ResolvedToolContext` registry routing.
- Let trusted bindings select their semantic runtime executor.
- Preserve `active_environment_id`, environment policy, workspace links, and
  handle-owned environment ids as explicit bounded facts.

### 4. Delete fused filesystem composition

- Remove environment routes from VFS/session filesystem construction.
- Delete `SessionFileSystem` if unused after the split.
- Remove route precedence, fused/host route variants, synthetic mixed listing,
  and same-state metadata.
- Ensure environment-only execution never touches VFS stores and VFS-only
  execution never connects to an environment.
- Update failure vocabulary.

### 5. Make skills explicitly VFS-owned

- Rename the active context key to `skills.catalog.vfs`.
- Add explicit VFS catalog identity/source to the catalog snapshot schema.
- Restrict the VFS catalog builder/model to snapshot and workspace roots.
- Remove generic target and host-location fields from the VFS path.
- Update catalog prompt/activation text to reference VFS tools.
- Retain automatic refresh and source-fingerprint behavior.

### 6. Keep prompts/instructions VFS-only

- Preserve pre-run VFS prompt refresh behavior.
- Make VFS provenance explicit in labels and documentation.
- Remove generic/session-filesystem wording and any environment projection
  hooks that imply future automatic prompt sourcing.

### 7. Update contracts and documentation

- Regenerate API schema/OpenRPC/reference artifacts if wire types changed.
- Regenerate/check both TypeScript consumers.
- Update current design/spec/profile examples and mark superseded roadmap
  routing decisions.

## Tests

### Toolset derivation

- VFS-only read configuration exposes only the four read-only VFS tools.
- VFS edit configuration exposes all seven VFS tools.
- Environment-only configuration exposes ordinary file/process tools and no
  VFS tools.
- A configuration with both features exposes both non-colliding families.
- OpenAI provider defaults use canonical OpenAI-oriented schemas.
- Anthropic provider defaults use Claude-Code-like schemas.
- Explicit Codex-like presentation remains supported.

### Routing and isolation

- The same path may contain different bytes in VFS and the active environment;
  each family reads only its own domain.
- VFS writes never mutate an environment filesystem.
- Environment writes never advance a VFS workspace head.
- VFS workspace writes retain snapshot commit effects and revision CAS.
- Snapshot links reject every VFS write operation.
- Environment-only calls perform zero VFS catalog/store reads.
- VFS-only calls perform zero environment resolver/host connection reads.
- Missing environment, missing filesystem capability, and read-only
  environment failures are structured and distinct.

### Determinism and concurrency

- A tool batch consumes the active environment id captured when the workflow
  scheduled it, including after activity retry.
- A later selection cannot redirect an in-flight environment operation.
- Activation/deactivation cannot mix with environment-dependent calls.
- VFS calls are not classified as environment-dependent.
- Existing job handles continue resolving their originating environment.
- Shared-workspace concurrent VFS mutations conflict through revision CAS
  rather than losing an update.

### Skills and instructions

- The VFS skill catalog uses `skills.catalog.vfs` and declares VFS source
  identity.
- VFS workspace-head changes continue refreshing the catalog before a new run.
- VFS skill prompt text directs the model to a VFS read tool.
- No environment path can enter the VFS catalog builder or source fingerprint.
- Environment selection does not alter VFS skill or instruction context.
- VFS instruction refresh retains current cross-session workspace behavior.

### Removal coverage

- No effective tool spec or invocation/event fixture contains a generic target.
- No hosted runtime path constructs a fused VFS/environment filesystem.
- No current documentation says VFS wins an environment path collision.

Run at least:

```bash
cargo test -p engine
cargo test -p tools
cargo test -p llm-runtime
cargo test -p temporal-workflow
cargo test -p temporal-server
cargo test -p api
```

After API wire changes:

```bash
cargo run -p api --bin export-schema
cd clients/typescript && npm install && npm run check
cd ../configurator-mcp && npm install && npm run check
```

Relevant ignored provider and Temporal live tests should be run after the
offline suites, with Temporal tests serialized according to `AGENTS.md`.

## Non-Goals

P113 does not:

- copy or materialize VFS content into an environment;
- capture environment files into a VFS snapshot;
- synchronize a VFS workspace and environment directory;
- provide a unified filesystem view;
- automatically source prompts or instructions from an environment;
- discover or publish an environment skill catalog;
- add workspace branching, merging, leases, or checkout ownership;
- add VFS copy/move/remove tools beyond the mirrored current surface; or
- make environment capabilities durable session state.

These can be designed independently later. Any future transfer feature must be
explicit about immutable snapshot inputs, shared-workspace revision conflicts,
environment destination ownership, and whether it mutates a shared workspace
head.

## Done When

- VFS access is possible only through the dedicated VFS tool family.
- Ordinary file/process/new-job operations use only the active environment.
- Environment-only sessions receive ordinary file tools without enabling VFS.
- VFS-only sessions retain read/edit, shared-workspace CAS, prompt refresh, and
  skill-catalog refresh behavior.
- No generic execution-target or reduced execution-requirement system remains.
- No runtime or projection fuses VFS routes with an environment filesystem.
- The current skill catalog is explicitly and exclusively VFS-owned.
- Provider-default OpenAI, Anthropic, and explicit Codex-like tool shapes remain
  covered by tests.
