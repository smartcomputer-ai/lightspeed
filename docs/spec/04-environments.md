# Environments

Design notes for making a Lightspeed agent aware of, and able to act against, a
VFS plus zero or more execution environments at a time.

Status: design plus preparatory tools refactor. This doc fixes the model, the
routing rules, the projection, and the open questions; it does not prescribe the
provider integrations.

## The problem

Almost every coding agent (and most agent harnesses) assume **one agent == one
filesystem == one process namespace**, because the agent runs *inside* a guest
OS. Lightspeed does not. A Lightspeed session has:

- an optional **VFS** — CAS-backed, durable, writable, possibly several snapshots
  and workspaces — but with **no process model**: you cannot `exec` "inside" a
  content-addressed tree. Prompts and skills usually live here, and often *only*
  here (not on any host disk).
- **zero or more environments** (sandboxes / remote hosts / attached hosts)
  reachable over the host protocol — real process namespaces with real shells.
- and, later, other surfaces (a browser, a connector).

So the agent needs two things it does not have today:

1. **Awareness.** A way to know what its filesystem looks like, which
   environments exist, what each can do (read? write? exec?), and — critically —
   that the VFS has no shell.
2. **Correct routing.** A way for a tool call to reach the right place without the
   runtime having to guess.

The concrete failure this refactor addresses: the hosted runtime used to map the
session's VFS mounts into a `host:local` target that had **no process executor**
(`crates/temporal-server/src/worker/session_tools.rs`). File tools work; an
`exec` against those same paths cannot. The agent has no way to know this in
advance, so it tries to `run_process` on a VFS path and loops on the failure.

## What already exists

This is not a green field. The low-level pieces are in place and the design
builds on them rather than replacing them.

- **Deterministic target identity.** The engine has `ToolExecutionTarget
  { namespace, id }`, `ToolTargetRequirement::{None, Optional, Required}`, and
  `ToolRoutingState.default_targets: BTreeMap<namespace, target>`
  (`crates/engine/src/core/components/tooling.rs`). Planning resolves the default
  target for a tool's namespace and **stamps `execution_target` onto each tool
  call before any side effect runs** (`resolve_tool_execution_target`, same
  file). This is the correct audit/replay boundary: the durable log records
  *which target* every effect went to.
- **Per-call target resolution in the runtime.** Each `ToolInvocationRequest`
  carries `execution_target: Option<ToolExecutionTarget>`. The executor resolves
  it through `ToolTargets` into either `FsToolContext` or
  `EnvironmentToolContext` (`crates/tools/src/runtime/inline.rs`,
  `crates/tools/src/targets.rs`). Crucially, the target is chosen *before* the
  call reaches the tool; `run_process` only resolves `cwd` within the
  already-selected environment context
  (`crates/tools/src/environment/tools/run_process.rs`). **The exec tool never inspects
  the command to pick an environment.** That is the seam this whole design rests
  on.
- **A capability-typed host wire protocol.** `host-protocol` / `host-client`
  define `HostConnectionSpec`, `HostTransport::{WebSocket, Http, Stdio, Ssh,
  Provider{provider_type}}`, `HostCapabilities` (fs read/write, process
  start/stdin/terminate/pty, output notifications), `HostScope::{Default,
  Session}`, and a normalized `HostPath`. The control plane already has
  `HostTargetCreateRequest::{Sandbox, AttachedHost, Provider}`,
  `SandboxTargetSpec`, and attach/close
  (`crates/host-protocol/src/control/targets.rs`). The sandbox/provider lifecycle
  abstraction is substantially built.
- **A fused filesystem primitive.** `SessionFileSystem`
  (`crates/tools/src/fs/session.rs`) routes by deepest matching prefix across
  generic `FileSystem` backends and exposes route metadata for projection.
  `MountedVfsFileSystem` (`crates/tools/src/fs/vfs.rs`) remains the VFS
  implementation over `VfsMountTable`, dispatching to snapshot/workspace
  filesystems and preserving workspace commit effects.
- **The skill catalog as the projection precedent.** A typed runtime fact
  (the skill catalog) is written to CAS, published into model context as a keyed
  `ContextEntryKind::SkillCatalog` entry, and rendered provider-neutrally into
  the prompt (`crates/tools/src/skills/catalog.rs`,
  `crates/llm-runtime/src/skill_prompts.rs`). Skill *activation* is a second
  entry kind (`SkillActivation { skill_id }`) that expands the chosen skill. This
  catalog/activation pattern is exactly what environments should copy.

## The model

The whole design reduces to a few orthogonal facts:

- **An optional VFS**, always available when present, served entirely by
  Lightspeed's fs tools. It may contain several snapshots and workspaces. It has
  **no shell**.
- **Zero or more environments** configured on the session.
- **At most one environment *active* at a time.** Activation is the selection
  mechanism — modeled on skill activation.
- **VFS is reached through fs tools only.** The shell of an active environment
  never sees VFS-only paths (skills, prompts). The agent reads those with
  `read_file`/`glob`, never by `cat`-ing them from a shell.

Everything else — routing, projection, the "same state" question — falls out of
these.

### Two layers, not one tree

The single clarifying idea: there are **two filesystem layers**, and conflating
them is what makes "where do paths live" confusing.

1. **The fs-tool layer** — what `read_file`/`write_file`/`edit_file`/`glob`/
   `grep`/`list_dir` see. This is a fused view that **Lightspeed's runtime owns
   and resolves**. It is the union of VFS routes (`/skills`, `/prompts`, a VFS
   `/workspace`) and, when an environment is active, that environment's own
   filesystem routes (`/repo` on the sandbox disk, etc.). VFS and the active
   environment **coexist here**. Lightspeed maps each path to its backend.
2. **The shell layer** — what `run_process` runs against. This is the active
   environment's **real OS filesystem**. It sees only what is physically on that
   box. It does **not** see VFS-only paths unless something materialized them
   there (we do not — see decisions). There is no shell layer at all when no
   environment is active.

The **overlap** is the set of paths where layer 1 and layer 2 agree (same path,
same bytes). For coding this must include the workspace: the agent edits files
with fs tools and runs them with the shell, and they must be the same files.
The overlap need **not** include skills/prompts — those live only in layer 1 and
never need a shell.

So "VFS fuses *into* the host's namespace" was the wrong framing. The correct
framing: **the fs-tool layer is the fusion of VFS and the active environment's
fs; the shell sees only the environment.** Skills/prompts being VFS-only is not a
special case — it is the normal state of layer-1-only routes.

### Why at most one active environment

Restricting to one active environment is what keeps routing sound, and it is a
deliberate constraint, not a limitation we regret.

A shell command's path is fixed by that environment's real OS, and we do not
control it. Two Linux sandboxes both root at `/`; both have `/workspace`,
`/tmp`, `/etc`. The bare path `/workspace/x` is a valid, *different* file in
each — the path alone cannot disambiguate. Windows makes it categorical, not just
ambiguous: `C:\workspace`, backslashes, drive letters, and case-insensitivity
have no sound embedding in a `/`-rooted POSIX tree. You cannot fuse two hosts'
shell namespaces into one tree even in principle.

With **one** active environment there is exactly one shell namespace, so the
problem vanishes: every exec call and every environment-fs path resolves against
the single active environment, unambiguously. The VFS adds only layer-1 routes,
which have no competing shell. This is the namespace the old combined host tool
context used to assume.

Concurrently active multiple environments would reintroduce the collision. If we
ever need it, the address of a file becomes the pair `(env_id, path)` rather than
a bare path, and selection must move onto the tools themselves (distinct
per-environment tool sets, or an explicit `env_id` argument). That is recorded as
an open question below — but the product does not need it, and the
one-active-environment rule is precisely how we avoid it.

### Routing rules

- **fs tools** resolve their path against the fused layer-1 view: VFS routes plus,
  when an environment is active, that environment's filesystem routes. Generalize
  the existing `MountedVfsFileSystem::resolve_mount` (deepest-prefix wins) so a
  resolved route can point at either a VFS source or the active environment's
  host filesystem. With one active environment the path is unambiguous; no
  per-call environment selection is needed.

  Constraint to respect: core does not parse provider tool JSON arguments, so it
  cannot stamp the final per-route backend target onto a file-tool call before
  execution. That is acceptable. For fs tools, `execution_target` denotes "the
  session fused filesystem service"; the runtime resolves the actual route and
  records it through model-visible output, structured `ToolEffect`s, and the
  catalog revision. Deterministic pre-routing per route would require a
  provider-visible target parameter or a core-visible structured fs operation —
  never shell/path inference.

- **exec tools** route to the **active environment**, carried by the existing
  `execution_target` and resolved before the call reaches the tool. No path
  inference, ever (a command like `python - <<'PY' … open("/workspace/x") … PY`
  exposes nothing parseable; by the time the file is opened the process is
  already running). If there is no active environment, exec is simply
  unavailable.

- **capability check, instructive failure.** If exec is attempted with no active
  environment (or against a non-exec target), fail fast with a structured error
  that says where exec *can* happen:

  > No execution environment is active. Your files live in the VFS (read/write
  > via file tools) but the VFS has no shell. Activate an environment to run
  > commands. Available: `sandbox-1` (Linux, cwd `/workspace`).

  That deterministic, capability-derived error is what actually breaks the
  agent's retry loop — far better than prose buried in a system prompt.

### Projection: VFS catalog + environment catalog + active environment

Mirror the skill mechanism, which already separates the *menu* (`SkillCatalog`)
from the *chosen, expanded* item (`SkillActivation`). Environments get the same
shape, with VFS handled as an always-present standing entry rather than a
catalog item — because VFS is never *selected*, it is simply always there:

- **`ContextEntryKind::VfsCatalog`** — a standing entry, published whenever VFS
  mounts change, describing the VFS routes the agent always has via fs tools
  (`/skills`, `/prompts`, `/workspace`, …) and stating plainly that the VFS has
  no shell. Always present when a VFS exists.
- **`ContextEntryKind::EnvironmentCatalog`** — the menu of *environments only*
  (not VFS). Lists each configured environment, its capabilities, and whether it
  is active. Published whenever the environment set or active selection changes.
- **`ContextEntryKind::EnvironmentActive`** — published when an environment is
  active, expanding it: its native paths, cwd, capabilities, and which fs routes
  overlap with the shell (the "same files" fact). This is the analogue of an
  activated skill's expanded body, and it is the natural home for **richer
  active-environment info later** (installed toolchains, detected project type,
  environment-specific guidance) — plan for it, ship it minimal.

This keeps the catalog purely a menu of selectable things and the VFS a standing
fact, which is the cleaner split. (The alternative — one combined entry holding
VFS + all environments — is viable but muddies "catalog = things you choose
among"; we prefer the separated form.)

Rendered text, VFS-only session (no environment active):

```text
Filesystem (virtual — file tools only, NO shell):
  /skills      read-only — skill library. Read SKILL.md before following a skill.
  /prompts     read-only — prompt library.
  /workspace   read/write — your durable working files.

No execution environment is active. There is no shell; commands cannot run.
```

Rendered text, one active environment (the coding case):

```text
Filesystem (file tools):
  VFS (no shell):
    /skills      read-only — skill library.
    /prompts     read-only — prompt library.
  sandbox-1 [ACTIVE] — Linux, exec available, cwd /workspace:
    /workspace   read/write — file-tool edits AND shell edits are the same files.
    /repo        read-only — on the sandbox disk.

Commands run in sandbox-1. /skills and /prompts are virtual (file tools only);
the shell cannot see them.
```

Whether a route's file-tool state equals its shell state is **not** an authoring
choice in the renderer — it is derived from `FsRoute.same_state_as_active_env`
(below). VFS routes resolve to `None` (no shell); the overlapping workspace
resolves to the active environment. The agent's correctness depends on this, so
it lives in the schema, not in prose.

### Schema

```text
VfsCatalog                       (standing context entry; always when VFS present)
  routes[]                       VFS-only fs routes (path, access, source); no shell

EnvironmentCatalogSnapshot       (the menu of environments only)
  schema_version
  revision                       gates republication — mirror
                                 `prepare_skill_catalog_publication`, which skips the
                                 UpsertContext when the new catalog CAS-ref equals the
                                 current one. `revision` is the readable counter; blob-ref
                                 equality is the actual change check.
  active_env_id                  Option<env_id> — the single active environment, if any
  environments[]

EnvironmentRecord
  env_id            stable handle, e.g. "sandbox-1"
  kind              Sandbox | AttachedHost
  capabilities      fs_read, fs_write, process_exec, process_stdin, network, persistent
  exec_target       Option<ToolExecutionTarget> — set for process-capable environments
  cwd               default working directory for exec
  status            Attaching | Ready | Degraded | Detached

EnvironmentActive                (published only when an environment is active)
  env_id
  fs_routes[]                    routes contributed by the active environment (layer 1)
  # room for richer info later: toolchains, project type, env-specific guidance

FsRoute
  path              where it appears in the fs-tool view, e.g. "/workspace"
  access            ReadOnly | ReadWrite
  source            VfsWorkspace | VfsSnapshot | HostFilesystem | FusedWorkspace
  same_state_as_active_env  Option<env_id>
                    set when edits through this route and shell edits in the active
                    environment are the same underlying state (the overlap). VFS-only
                    routes are None. This is what the renderer reads to say "same files".
```

Design constraints, consistent with the P51 architecture rules:

- **Keep these entries thin.** Routes, capability booleans, cwd, status, and
  the same-state fact. Transport, credentials, `HostConnectionSpec`, provider
  specs, leases — **none of that enters context or the session log.** It is
  runtime/deployment config, exactly like LLM transport config stays out of
  `ModelSelection`. Process-capable records reference an `exec_target`; the
  runtime resolves it to a live `EnvironmentToolContext` / host connection
  through `ToolTargets`.
- **`capabilities` is the core's mirror of `HostCapabilities`.** We do not make
  `engine` depend on `host-protocol`; we mirror the booleans the agent and the
  router need.
- **The engine only records target identity for side effects.** These entries
  carry semantic facts; they do not manage lifecycle. Attach/detach/activate is
  driven by the runtime, which owns provisioning.

### Catalog first; core state later

Start with these as **runtime-owned** snapshots written to CAS and published into
context, reusing `default_targets["env"]` routing for the active
environment. This proves the entire agent-facing model without forcing the
deterministic engine to own lifecycle facts it does not yet branch on.

Promote into a core `EnvironmentState` (sibling of `tooling`/`context`, reduced
from explicit events) only when the engine needs to branch on environment facts,
or clients need an event-sourced environment timeline. If promoted, likely
commands are `AttachEnvironment`, `DetachEnvironment`, `SetEnvironmentStatus`, and
`ActivateEnvironment`.

The activation command (not an either/or): the environment-namespace default
`default_targets["env"]` remains the **single deterministic source of truth**
for where exec lands. `ActivateEnvironment { env_id }` is the only author-facing
command, and it is **pure sugar** — it resolves `env_id → exec_target` against the
catalog and **lowers to** `SetDefaultToolTarget { namespace: "env", target }`,
and republishes the catalog + active-environment entries. There is never a second
default to keep in sync. Deactivation clears the environment default and the active
entry.

Sandbox standup surfaces as: runtime provisions/handshakes a host connection,
capability negotiation happens outside core, the runtime records the ready
environment in the catalog; the agent (or a policy) activates it.

## Open question: VFS ↔ environment fusion for the workspace (decide later)

The one fact that *must* be decided before shipping coding sessions: for the
overlapping workspace, **are file-tool edits and shell edits the same state?**
The model above is correct regardless of mechanism — only how the workspace route
is backed, and therefore `same_state_as_active_env`, changes.

Options, with consequences:

1. **Mount VFS workspace into the environment.** `/workspace` in the environment
   is backed by the VFS workspace (FUSE / network fs /
   `MountedVfsFileSystem`-as-host-fs). One backing store; edit-via-tools /
   run-via-shell over the same files, live. → route `source: FusedWorkspace`,
   `same_state_as_active_env: Some(active)`. Cleanest agent story; the existing
   fused-fs composition points this way. Cost: a real mount mechanism inside the
   environment, and acceptable build/test IO latency.
2. **Materialize / sync before exec.** Snapshot the VFS workspace into the
   environment fs before a command runs, snapshot results back. → `source:
   VfsWorkspace`, `same_state_as_active_env: None` until a sync completes, so the
   renderer honestly says "sync first." Simple transport; cost is staleness
   windows and an explicit flush concept the agent must understand.
3. **Environment fs is primary; snapshot back to VFS.** The writable workspace
   *is* the environment; VFS is the persistence/CAS layer behind it. → `/workspace`
   route `source: HostFilesystem` on the environment, `same_state_as_active_env:
   Some(active)` by construction. Simplest exec story. Cost: VFS-only sessions
   need a different primary, and durability depends on snapshot cadence.

In all three, VFS-only routes (`/skills`, `/prompts`) are unaffected: `source:
Vfs*`, `same_state_as_active_env: None`, served by fs tools, invisible to the
shell. That is settled (see decisions), not part of this open question.

Recommended default *for coding sessions* (not an invariant): provision one
environment whose `/workspace` overlaps the VFS workspace (option 1 or 3), and
keep the VFS-only, no-environment session first-class — cheap, durable, no
container, file-tools-only. That is a Lightspeed differentiator and the model
must not foreclose it.

## Open question: multiple concurrently-active environments (deferred, maybe never)

The model fixes **at most one active environment**, which is what keeps paths
unambiguous (see "Why at most one active environment"). Supporting several
*active at once* would require addressing files as `(env_id, path)` and moving
environment selection onto the tools — either distinct per-environment tool sets
(self-documenting, parallel-safe, but multiplies the tool list) or an explicit
`env_id` argument on every fs and exec tool (one tool set, more model burden).
`ToolTargetRequirement::Required { namespace: "env" }` only resolves the
namespace default, so either path needs a tool-level routing extension first.

Defer until there is a concrete need. The product shape — one active environment
at a time, switched by activation — does not reach this.

## Build vs. separate repo

Keep the host protocol, the environment/session API, the catalog/projection, and
target resolution **in Lightspeed**. The boundary is already clean: a sandbox
provider is "an implementation that provisions a backend and speaks the host
protocol" via the `HostTransport::Provider{provider_type}` /
`HostTargetCreateRequest::Provider` arms that already exist. Provider
*implementations* may live elsewhere if they need an independent release cadence
or a non-Rust runtime; the `host-protocol` crate is exactly the seam that makes
that a later, cheap move. Do not start by extracting the standup into a separate
product-shaped repo — that pulls the agent-facing model out with it for no gain.

The engine stays where the architecture rules put it: it knows semantic target
identity, not lifecycle, credentials, leases, workers, or provider APIs.

## Implementation path (proposed)

1. **Runtime projection + instructive failures first** (no new provider, no new
   core state). Add `ContextEntryKind::{VfsCatalog, EnvironmentCatalog,
   EnvironmentActive}`, the snapshot schemas, and publish them from gateway/worker
   code. Make exec fail with the "no active environment / VFS has no shell" error
   when appropriate. Prove the agent-facing design against the existing
   `InlineToolRuntime` with VFS + a local environment.
2. **A `SessionEnvironmentManager` in `temporal-server`** that composes VFS mounts
   and the active environment target into the `ToolTargets` the runtime resolves
   against, and republishes the entries whenever VFS mounts, the
   environment set, or the active selection change. `ActivateEnvironment` lowers
   to the `env` default target.
3. **Change hosted VFS behavior** from "VFS pretends to be `host:local` with no
   executor" to "VFS is the layer-1-only filesystem; an active environment
   supplies the shell and its own routes." This is the fix for the original
   failure.
4. **Session API for environments:** list / create-or-attach / activate /
   deactivate / close. Keep provider-specific sandbox specs opaque at the API
   boundary, like provider params.
5. **Optional core promotion.** Add `EnvironmentState` + attach/detach/activate
   events only once the runtime projection has stabilized or deterministic
   planning needs environment facts.
6. **One provider adapter, early but minimal** — only to exercise the protocol
   boundary. The agent should learn "a sandbox with exec and `/workspace`", never
   the specific provider.

## North star

The model should always know its topology — that it has a VFS reachable by file
tools with no shell; whether an execution environment is active and what it can
do; and, for the workspace, whether file edits and shell edits are the same
state. The engine should only record the deterministic target identity used for
each side effect. That preserves Lightspeed's advantage over guest-OS agents — a
durable virtual filesystem coexisting with a swappable, capability-typed
execution environment — without turning the deterministic core into a sandbox
manager.
