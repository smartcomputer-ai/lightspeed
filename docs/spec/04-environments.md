# Environments

Design notes for making a Lightspeed agent aware of, and able to act against, a
VFS plus zero or more execution environments at a time.

Status: design implemented through the first real bridge-backed environment
path. P75-P81 landed the `fs`/`env` tool namespace split, runtime projection,
`SessionEnvironmentManager`, active-environment runtime wiring, public session
environment APIs, provider registry, and a standalone `host-bridge` runner. An
ignored live test now starts the gateway/worker, spawns `host-bridge`,
attaches/activates it, and verifies that an agent can write through exec and
read the same guest filesystem through file tools.

What remains open is narrower: a real sandbox/VM provider, provider lifecycle
hardening (leases, stale/offline cleanup, auth/policy), and future
computer-use/browser surfaces. Multiple concurrently-active environments remain
intentionally deferred.

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

So the design needs two things:

1. **Awareness.** A way to know what its filesystem looks like, which
   environments exist, what each can do (read? write? exec?), and — critically —
   that the VFS has no shell.
2. **Correct routing.** A way for a tool call to reach the right place without the
   runtime having to guess.

The concrete failure this refactor addressed: the hosted runtime used to map the
session's VFS mounts into a `host:local` target that had **no process executor**
(`crates/temporal-server/src/worker/session_tools.rs`). File tools worked; an
`exec` against those same paths could not. The agent had no way to know this in
advance, so it tried to `run_process` on a VFS path and looped on the failure.

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
- **A runtime provider registry and first bridge provider.**
  `crates/environments` plus the PostgreSQL migration in
  `crates/store-pg/migrations/006_environments.sql` store provider
  records, observed targets, and session environment bindings. The gateway owns
  the public session environment API and uses host-protocol controllers to
  create, attach, and close targets. `crates/host-bridge` is the first real
  provider: a standalone binary that registers, heartbeats, exposes a
  host-protocol WebSocket controller/data plane, and represents the OS it is
  running in as an attached host.
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
   `grep`/`list_dir` see. This is a composed view that **Lightspeed's runtime
   owns and resolves**. It is the union of VFS routes (`/skills`, `/prompts`,
   session workspaces, etc.) and, when an environment is active, that
   environment's own filesystem routes (often `/` for an attached host, or a
   provider-chosen project root). VFS and the active environment **coexist here**.
   Lightspeed maps each path to its backend. If a VFS route and an environment
   route collide, the **VFS route wins**.
2. **The shell layer** — what `run_process` runs against. This is the active
   environment's **real OS filesystem**. It sees only what is physically on that
   box. It does **not** see VFS-only paths unless something materialized them
   there (we do not — see decisions). There is no shell layer at all when no
   environment is active.

The **overlap** is the set of paths where layer 1 and layer 2 agree (same path,
same bytes). For now, overlap exists only for environment-backed routes that are
not shadowed by VFS. There is **no implicit sync** between a VFS workspace and an
environment filesystem. If a VFS path and an environment path have the same
name, file tools see the VFS path and shell commands see the environment path;
the projection must therefore report `same_state_as_active_env: None` for the
VFS route. This means VFS mounts should stay out of the way of the active
environment's working directory unless that separation is intentional.

So "VFS fuses *into* the host's namespace" was the wrong framing. The correct
framing: **the fs-tool layer is a VFS-first composition of VFS and the active
environment's fs; the shell sees only the environment.** Skills/prompts being
VFS-only is not a special case — it is the normal state of layer-1-only routes.

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

- **fs tools** resolve their path against the composed layer-1 view: VFS routes
  plus, when an environment is active, that environment's filesystem routes.
  VFS routes have precedence over environment routes on collision; environment
  routes fill the remaining path space. Generalize the existing
  `MountedVfsFileSystem::resolve_mount` so a resolved route can point at either
  a VFS source or the active environment's host filesystem, with VFS-first
  precedence. With one active environment the path is unambiguous; no per-call
  environment selection is needed.

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
  VFS (no shell, wins on path collisions):
    /skills      read-only — skill library.
    /prompts     read-only — prompt library.
  bridge-local [ACTIVE] — Linux, exec available, cwd /Users/lukas/dev/app:
    /            read/write — attached host filesystem, except VFS-shadowed paths.

Commands run in bridge-local. /skills and /prompts are virtual (file tools only);
the shell cannot see them. File-tool relative paths resolve against the fs cwd;
when that cwd is not shadowed by VFS, file-tool edits and shell edits are the
same files.
```

Whether a route's file-tool state equals its shell state is **not** an authoring
choice in the renderer — it is derived from `FsRoute.same_state_as_active_env`
(below). VFS routes resolve to `None` (no shell). Environment routes resolve to
the active environment only where they are not shadowed by VFS. The agent's
correctness depends on this, so it lives in the schema, not in prose.

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
  path              where it appears in the fs-tool view, e.g. "/skills" or "/"
  access            ReadOnly | ReadWrite
  source            VfsWorkspace | VfsSnapshot | HostFilesystem | FusedWorkspace
                    (FusedWorkspace is reserved for a future explicit fusion mode)
  same_state_as_active_env  Option<env_id>
                    set when edits through paths that resolve to this route and shell
                    edits in the active environment are the same underlying state.
                    VFS routes and VFS-shadowed paths are None. This is what the
                    renderer reads to say "same files".
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

The current implementation keeps these as **runtime-owned** snapshots written to
CAS and published into context, reusing `default_targets["env"]` routing for the
active environment. This proves the entire agent-facing model without forcing
the deterministic engine to own lifecycle facts it does not yet branch on.

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

Environment standup surfaces as: a provider registers and heartbeats; the
gateway provisions or attaches a host-protocol target; capability negotiation
happens outside core; the runtime records the ready environment in the catalog;
the agent, user, or policy activates it. P81 implements the attached-host bridge
case. A true sandbox provider still needs to be implemented.

## Decision: VFS and environment filesystems stay separate for now

Do **not** sync VFS workspaces into environments, mount VFS into environments, or
snapshot environment workspaces back to VFS as part of the default environment
flow. The VFS remains Lightspeed-owned durable state reachable by file tools. An
environment remains a real OS filesystem reachable by file tools and shell only
through its advertised host-protocol routes.

The routing rule is:

1. VFS routes win on path collision.
2. Environment routes fill paths not claimed by VFS.
3. Shell commands always see only the environment filesystem.
4. `same_state_as_active_env` is set only for environment-backed fs-tool routes
   that are not shadowed by VFS.

Operational consequence: VFS should stay out of the way of the attached
environment. Use reserved paths like `/skills` and `/prompts` for VFS-only
resources. Avoid mounting a VFS workspace at the active environment cwd unless
the intended behavior is that file tools and shell commands intentionally see
different states at the same path.

Future workspace fusion remains possible, but it should be introduced as an
explicit mode with explicit projection semantics. Until then, there is no hidden
durability or sync boundary between VFS and an environment.

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
target resolution **in Lightspeed**. The boundary is already clean: a provider is
"an implementation that provisions or exposes a backend and speaks the host
protocol" via the `HostTransport::Provider{provider_type}` /
`HostTargetCreateRequest::{Sandbox, AttachedHost, Provider}` arms. Provider
*implementations* may live elsewhere if they need an independent release cadence
or a non-Rust runtime; the `host-protocol` crate is exactly the seam that makes
that a later, cheap move. The first implementation lives in this repository as
`crates/host-bridge`, but it is still a standalone binary with its own lifecycle,
not part of the Lightspeed CLI.

The engine stays where the architecture rules put it: it knows semantic target
identity, not lifecycle, credentials, leases, workers, or provider APIs.

## Implementation status

Implemented:

1. **Environment-ready tools refactor (P75).** Split file tools from environment
   action tools, renamed the old host target namespace out of model-facing
   routing, kept shared file tools on the generic `FileSystem` trait, and kept
   host-protocol as the backend wire boundary.
2. **Runtime projection (P76).** Added
   `ContextEntryKind::{VfsCatalog, EnvironmentCatalog, EnvironmentActive}`, the
   projection schemas, provider-neutral rendering, and instructive no-shell
   failures.
3. **Session environment manager and active runtime wiring (P77-P78).** Added one
   runtime owner that composes VFS mounts, active environment routes, context
   republication, and `ToolTargets`; activation lowers to the `env` default
   target used by process tools.
4. **Public session environment API (P79-P80).** Added list/read/create/attach/
   activate/deactivate/close, provider registry records, provider
   register/heartbeat/unregister, and gateway lifecycle wiring through
   host-protocol controllers.
5. **First real provider (P81).** Added the standalone `host-bridge` binary and
   CLI helpers. The live bridge test proves a session can attach and activate a
   bridge provider and that exec/file tools can operate on the same attached host
   filesystem.
6. **VFS-first collision precedence.** Implemented the decision above: VFS mount
   routes shadow active-environment routes on collision, active environments can
   be mounted broadly, and file-tool cwd follows the active environment cwd when
   one is active.

Remaining:

1. **Sandbox/VM provider.** Implement a provider that actually provisions isolated
   coding environments rather than attaching to an already-running OS.
2. **Provider lifecycle hardening.** Tighten leases, stale/offline sweeping,
   auth/policy, observability, and cleanup semantics for production operation.
3. **Optional core promotion.** Add `EnvironmentState` + attach/detach/activate
   events only once deterministic planning needs environment facts or clients
   need an event-sourced environment timeline.
4. **Additional surfaces.** Computer-use/browser environments remain future
   extensions of the environment action model.

## North star

The model should always know its topology — that it has a VFS reachable by file
tools with no shell; whether an execution environment is active and what it can
do; and which fs-tool paths, if any, are the same state as the active
environment's shell. The engine should only record the deterministic target
identity used for each side effect. That preserves Lightspeed's advantage over
guest-OS agents — a durable virtual filesystem coexisting with a swappable,
capability-typed execution environment — without turning the deterministic core
into a sandbox manager.
