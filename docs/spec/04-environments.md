# Environments

Design notes for making a Lightspeed agent aware of, and able to act against,
more than one execution environment at a time.

Status: design. No code yet. This doc fixes the model, the routing rules, the
projection, and the open questions; it does not prescribe the provider
integrations.

## The problem

Almost every coding agent (and most agent harnesses) assume **one agent == one
filesystem == one process namespace**, because the agent runs *inside* a guest
OS. Lightspeed does not. A Lightspeed session can have:

- a VFS workspace (CAS-backed, durable, writable, but with **no process model** —
  you cannot `exec` "inside" a content-addressed tree),
- one or more sandboxes / remote hosts / attached hosts reachable over the host
  protocol (real process namespaces),
- read-only snapshot mounts (reference material),
- and, later, other surfaces (a browser, a connector).

So the agent needs two things it does not have today:

1. **Awareness.** A way to know which environments exist, what each can do
   (read? write? exec? persistent?), and where each lives in the path namespace.
2. **Correct routing.** A way for a tool call to reach the *right* environment —
   without the runtime having to guess.

The concrete failure today: the hosted runtime maps the session's VFS mounts into
a `host:local` target that has **no process executor**
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
  it to a `HostToolContext` via `HostToolTargets::resolve`
  (`crates/tools/src/host/executor.rs`, `targets.rs`). Crucially, the target is
  chosen *before* the call reaches the tool; `run_process` only resolves `cwd`
  *within* the already-selected target's filesystem
  (`crates/tools/src/host/tools/run_process.rs`). **The exec tool never inspects
  the command to pick a host.** That is the seam this whole design rests on.
- **A capability-typed host wire protocol.** `host-protocol` / `host-client`
  define `HostConnectionSpec`, `HostTransport::{WebSocket, Http, Stdio, Ssh,
  Provider{provider_type}}`, `HostCapabilities` (fs read/write, process
  start/stdin/terminate/pty, output notifications), `HostScope::{Default,
  Session}`, and a normalized `HostPath`. The control plane already has
  `HostTargetCreateRequest::{Sandbox, AttachedHost, Provider}`,
  `SandboxTargetSpec`, and attach/close
  (`crates/host-protocol/src/control/targets.rs`). The sandbox/provider lifecycle
  abstraction is substantially built.
- **A fused filesystem primitive.** `MountedVfsFileSystem`
  (`crates/tools/src/host/fs/vfs.rs`) already resolves a path against an ordered
  mount table (deepest-prefix-wins) and dispatches to a snapshot or workspace
  filesystem. `VfsMountAccess::{ReadOnly, ReadWrite}`. Path-based fusion exists
  *within* the VFS namespace today.
- **The skill catalog as the projection precedent.** A typed runtime fact
  (the skill catalog) is written to CAS, published into model context as a keyed
  `ContextEntryKind::SkillCatalog` entry, and rendered provider-neutrally into
  the prompt (`crates/tools/src/skills/catalog.rs`,
  `crates/llm-runtime/src/skill_prompts.rs`). The agent reads it and decides what
  to do. This is exactly the shape the environment catalog should copy.

## What is missing

The missing abstraction is not "host" and not "sandbox." It is a **session-scoped
environment catalog**, plus the discipline of routing exec by selected target
rather than by inferred path.

The first useful increment does not have to be a new deterministic core
component. A runtime-owned catalog, published into context and reflected in
better tool errors, can prove the model while reusing the existing
`default_targets["host"]` routing. Promote environment facts into core state only
once the engine needs to branch on them or clients need an event-sourced
environment timeline.

Three gaps, concretely:

1. **Targets are config, not an agent-visible concept.** `default_targets` is set
   once at startup and is **never projected into the prompt**. The agent cannot
   see that `host:local` vs `host:sandbox-1` exist, what each can do, or where
   its filesystem lives.
2. **The two namespaces are never reconciled.** VFS mounts live in a path-keyed
   `VfsMountTable`; host targets live in a namespace-keyed `default_targets`.
   Nothing produces a single map the agent reasons over: "`/workspace` is a
   writable VFS (no shell); `/repo` is a read-only snapshot; exec happens in
   `sandbox-1`."
3. **The rich capability model and the thin core identifier are not linked.**
   The core knows the target *id*; it does not carry `HostCapabilities`. Even if
   we projected targets, we would project bare ids with no "can it exec / is it
   writable / where is its root" semantics.

## The core insight: route exec by target, never by path

The single most important rule:

> **The runtime must not infer exec routing from the shell command.**

A command like

```
python - <<'PY'
from pathlib import Path
print(list(Path("/workspace/src").glob("*.rs")))
PY
```

does not expose stable, parseable information about which environment it touches.
By the time Python opens a file the process is already running *somewhere*.
Parsing shell/Python/node/ruby to infer path intent is a losing design, and it is
unnecessary: the engine already records `execution_target` on every call.

This forces a **split by tool modality**, not one uniform routing rule:

| Modality | Routes by | Why |
|---|---|---|
| Runtime-mediated **fs** (`read_file`, `write_file`, `edit_file`, `glob`, `grep`, `list_dir`) | **path** via the fused mount table | the path is a structured argument; Lightspeed owns the operation; this is where VFS/host fusion lives |
| **exec / shell** (`run_process`, stdin) | **selected execution environment** (one per call) | the path is opaque inside the command; the *process namespace* is the unit of choice |

If we ever find ourselves wanting to inspect a command to decide where it runs,
the session environment model is wrong — fix the model, not the parser.

A corollary that the agent must be told plainly: **file-tool edits and shell
edits are only the same state if the environments are fused.** The worst outcome
is not "exec on a VFS path fails" — it is silently telling the agent "your files
are in VFS at `/workspace`, but exec happens elsewhere" when those are different
filesystems. That makes the agent wrong even when every call "succeeds." This is
why fusion (below) is load-bearing, not cosmetic.

## The model

### Environment catalog first; core state later

Start with a runtime-owned `EnvironmentCatalogSnapshot`, written to CAS and
published into model context as a first-class
`ContextEntryKind::EnvironmentCatalog` keyed entry. This follows the
skill-catalog pattern without forcing the deterministic engine to own lifecycle
facts it does not yet use for planning.

If environment facts later need to participate in deterministic branching,
client event streams, or replayed session state, promote this snapshot into an
`EnvironmentState` sibling of `tooling`/`context`, reduced from explicit
environment events.

```text
EnvironmentCatalogSnapshot
  schema_version
  revision
  default_exec_env_id
  environments[]
  fs_routes[]
  warnings[]

EnvironmentRecord
  env_id            stable handle, e.g. "workspace", "sandbox-1"
  kind              Vfs | Sandbox | RemoteHost | AttachedHost | Connector | Browser
  capabilities      fs_read, fs_write, process_exec, process_stdin, network, persistent
  exec_target       Option<ToolExecutionTarget> — set only for process-capable targets
  cwd               default working directory for exec (when process_exec)
  status            Attaching | Ready | Degraded | Detached
  description_ref   Option<BlobRef> — agent-facing blurb, CAS-backed like skill docs

FsRoute
  path              where it appears in the fused fs tree, e.g. "/workspace"
  env_id            owning environment
  access            ReadOnly | ReadWrite
  source            VfsWorkspace | VfsSnapshot | HostFilesystem | FusedWorkspace
  same_state_as_exec_env_id Option<env_id>
```

Design constraints, consistent with the P51 architecture rules:

- **Keep the catalog thin.** Ids, path routes, capability booleans, cwd, status,
  and whether file-tool state matches exec state. Transport, credentials,
  `HostConnectionSpec`, provider specs, leases — **none of that enters the
  catalog or session log.** It is runtime/deployment config, exactly like LLM
  transport config stays out of `ModelSelection`. Process-capable records
  reference an `exec_target`; the runtime resolves that target to a live
  `HostToolContext` / host connection the same way `HostToolTargets::resolve`
  does today.
- **`capabilities` is the core's mirror of `HostCapabilities`.** We do not make
  `engine` depend on `host-protocol`; we mirror the booleans the agent and the
  router need. This is the piece that closes gap (3): the agent sees
  `process_exec: false` on the VFS workspace and `process_exec: true` on the
  sandbox.
- **The engine still only records target identity for side effects.** The
  environment catalog is a catalog of semantic facts; it does not manage
  lifecycle. Attaching/detaching is driven by the runtime, which owns
  provisioning.

If promoted into core, likely commands/events are `AttachEnvironment`,
`DetachEnvironment`, `SetEnvironmentStatus`, and `SetDefaultExecEnvironment`.
`SetDefaultExecEnvironment` should be defined as sugar over the existing
`SetDefaultToolTarget { namespace: "host", target }`, or as its replacement
source of truth — not a second default. Sandbox standup would surface as:
runtime provisions/handshakes a host connection, capability negotiation happens
outside core, then the runtime records/publishes the ready environment.

### Routing rules, derived from the model

- **fs tools:** build one fused `MountTable` as the union of VFS mounts and host
  roots — each route contributes `(path → env/source/access/capabilities)`.
  Generalize the existing `MountedVfsFileSystem::resolve_mount` (deepest-prefix
  wins) so a resolved route can point at either a VFS source or a host
  filesystem. The path *is* the routing decision; no separate "which target"
  question for fs ops.

  Important constraint: core does not parse provider tool JSON arguments, so it
  cannot stamp the final per-mount backend target onto a file-tool call before
  execution. That is acceptable. For fs tools, `execution_target` can mean "the
  session fused filesystem service" (or the existing default host target), while
  the runtime records the actual resolved route through model-visible output,
  structured `ToolEffect`s, and the environment catalog revision. If we ever need
  deterministic pre-routing per mount, we need a provider-visible target
  parameter or a core-visible structured fs operation, not shell/path inference.
- **exec tools:** route to a **selected execution environment**, one per call,
  carried by the existing `execution_target` and resolved before the call reaches
  the tool. There must be a **default exec environment** at session/run scope so
  the model is not choosing on every call.
- **capability check, instructive failure:** if an exec call resolves to an
  environment with `process_exec: false` (e.g. a VFS-only mount became the
  default by mistake), fail fast with a structured, instructive error that names
  the alternatives:

  > `/workspace` is a virtual workspace (read/write) with **no process
  > execution**. Exec environments: `sandbox-1` (cwd `/workspace`). Run commands
  > there.

  That deterministic, capability-derived error is what actually breaks the
  agent's retry loop. Models correct from a good error far more reliably than
  from prose buried in a system prompt.

### Multi-environment exec selection

For the common case — **one** exec environment — the model-visible
`run_process` arguments carry no target and core routes to the session default.
The internal `ToolInvocationRequest` still carries the resolved
`execution_target`. The VFS workspace simply *is not* an exec environment (no
shell), so there is no ambiguity and no misrouting risk. Zero agent effort.

For the rarer multi-environment case, prefer, in order:

1. **Distinct model-visible tools** — `exec_in_sandbox`, `exec_on_server`. These
   are self-documenting in the provider tool list and **cannot be misrouted on
   parallel calls.** This is the recommended surface, but it requires a
   tool-specific routing extension first: either a fixed execution target on
   `ToolSpec`/`ToolBinding`, or generated wrapper tools that lower to
   `run_process` with a fixed `ToolExecutionTarget`. The current
   `ToolTargetRequirement::Required { namespace: "host" }` only resolves the
   namespace default.
2. **A deliberate `environment_select` action** that changes the durable default
   exec environment for subsequent turns (recorded as a command, so it is in the
   log and replayable — not a hidden mutable "current host").
3. **A free-form `target_id`/`env_id` parameter on exec** — only as an advanced
   surface under real multi-target pressure. Avoid making this the primary UX: it
   adds model burden and is easy to misroute when calls run in parallel.

Do **not** depend on a mutable runtime "current host" that is not recorded into
the tool call. Whatever selection mechanism is used, the chosen target must land
on the `ToolInvocationRequest` and thus in the log.

### Projection: the environment catalog

Copy the skill-catalog mechanism. On any change to VFS mounts, attached host
targets, or default exec routing, the runtime renders a catalog into a typed
`ContextEntryKind::EnvironmentCatalog` context entry. This is a context
extension, not a requirement that the full environment lifecycle becomes core
state.

Rendered text the agent always sees — capability-honest, naming where exec lives:

```text
Environments (your filesystem and where commands run):

  /workspace   virtual workspace — read/write, NO command execution.
               Your file-tool edits persist here.
  /repo        snapshot — read-only. Reference only; not visible to the shell.
  sandbox-1    Linux sandbox — exec available, cwd /workspace.
               Run builds/tests here.

File tools route by path automatically. Commands run in sandbox-1.
[If fused:] Files you edit in /workspace are the same files sandbox-1 sees at /workspace.
[If not fused:] sandbox-1 has its own filesystem; sync edits before running them.
```

The bracketed line is determined by the fusion decision below and must be stated
explicitly — the agent's correctness depends on knowing whether file-tool state
and shell state are the same.

## Open question: VFS ↔ sandbox fusion (decide later)

Deferred deliberately. The mechanism is undecided, but the model above is
correct regardless of which we pick — only the catalog's bracketed sentence and
the sync behavior change. The fact that *must* be decided before shipping coding
sessions: **are file-tool edits and shell edits the same state?**

Options, with consequences:

1. **Mount VFS into the sandbox.** `/workspace` in the sandbox is backed by the
   VFS workspace (e.g. FUSE / network fs / `MountedVfsFileSystem`-as-host-fs).
   One logical file set; edit-in-VFS / run-in-sandbox over the same files.
   Cleanest agent story; the existing fused-fs composition already points this
   way. Cost: needs a real mount mechanism inside the sandbox and acceptable
   latency for build/test IO.
2. **Materialize / sync before exec.** Snapshot the VFS workspace into the
   sandbox filesystem before a command runs, and snapshot results back. Simple
   transport; no live mount. Cost: explicit sync points, staleness windows, and
   the agent must understand "edits are flushed before runs."
3. **Sandbox filesystem is primary; snapshot back to VFS.** The writable
   workspace *is* the sandbox; VFS becomes the persistence/CAS layer behind it.
   Simplest exec story; matches "provision one workspace execution environment."
   Cost: VFS-only (no-sandbox) sessions need a different primary, and durability
   now depends on snapshot cadence.

Recommended default *for coding sessions* (not an invariant): provision **one**
workspace execution environment that has both `process_exec` and the active
workspace path, and let VFS be the snapshot/persistence layer — option 1 or 3.
But keep the VFS-only, no-sandbox session a first-class shape: cheap, durable, no
container, file-tools-only. That is a Lightspeed differentiator and the model
must not foreclose it. So: "one default exec environment" is the coding default,
not a universal rule.

## Build vs. separate repo

Keep the host protocol, the environment/session API, the environment catalog, and
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

1. **Runtime catalog + instructive failures first** (no new provider, no new
   environment state). Add `ContextEntryKind::EnvironmentCatalog`, the
   `EnvironmentCatalogSnapshot` schema, and publish it into a keyed context entry
   from gateway/worker code. Improve host tool failures so a process request
   against a non-exec environment names the available exec alternatives. Prove
   the agent-facing design against the existing `InlineHostToolRuntime` with
   local host + VFS.
2. **A `SessionEnvironmentManager` in `temporal-server`** that composes VFS
   mounts, host-protocol targets, and (later) provider-backed sandboxes into the
   `HostToolTargets` the runtime resolves against, and publishes the environment
   catalog whenever VFS mounts or host targets change.
3. **Change hosted VFS behavior** from "VFS pretends to be `host:local`" to "VFS
   is a filesystem route; executable targets are separate environments." This is
   the fix for the original failure.
4. **Session API for environments:** list / create-or-attach / select-default /
   close. Keep provider-specific sandbox specs opaque at the API boundary, like
   provider params.
5. **Optional core promotion.** Add `EnvironmentState` plus attach/detach/status
   events only once the runtime catalog has stabilized or deterministic planning
   needs environment facts.
6. **One provider adapter, early but minimal** — only to exercise the protocol
   boundary. The agent should learn "a sandbox with exec and `/workspace`", never
   the specific provider.

## North star

The model should always know the topology — what environments exist, what each
can do, where the shell runs, and whether file edits and shell edits are the same
state. The engine should only record the deterministic target identity used for
each side effect. That preserves Lightspeed's advantage over guest-OS agents
(multiple, heterogeneous, capability-typed environments per session) without
turning the deterministic core into a sandbox manager.
