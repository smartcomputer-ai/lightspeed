# P83: Fleet Subagent Control Plane

**Status**
- Proposed 2026-06-23.
- Builds on **P82 (Session Graph — Clone, Fork, And Links)** for the underlying
  clone/fork/link store primitives, the Temporal-backed `AgentSessionWorkflow`,
  the `api` session/run surface, environment/provider work from P79-P81, and
  `docs/spec/05-subagents-reference-study.md`.
- This is the agent-facing control plane. The storage/algorithm foundation it
  sits on is owned by P82; this doc assumes those store methods exist.

## Goal

Let an agent safely create, configure, task, inspect, and cancel other agents.

The first implementation should support the happy path:

1. A parent agent calls a small Fleet tool such as `agent_spawn`.
2. Lightspeed validates policy and reserves a child identity.
3. Lightspeed creates the child as an ordinary session backed by its own
   `AgentSessionWorkflow`, cloning or forking the source via the P82 primitives.
4. Lightspeed applies the child run configuration.
5. Lightspeed starts the child run with the requested task.
6. The parent receives a durable child handle it can inspect or task later.

The model should not receive the raw `session/*`, `run/*`, environment, MCP,
auth, VFS, and blob APIs as separate tools. It should receive a small Fleet
control surface that compiles intent into those lower-level API operations and
the P82 clone/fork/link primitives.

## Relationship To P82

P82 provides the primitives; P83 provides the agent surface and the runtime
service that drives them:

| Concern | P82 (foundation) | P83 (this doc) |
|---|---|---|
| `sessions` lineage columns, `session_links` table | owns | consumes |
| `create_cloned_session` / `create_forked_session` / link CRUD | owns | calls |
| Fork read resolution + cut-point helper | owns | calls |
| `agent_*` model-visible tools | — | owns |
| Fleet service (validate, reserve, admit run) | — | owns |
| Spawn idempotency | — | owns |
| Capability policy / link enforcement | — | owns (deferred) |
| Resource share-vs-isolate knobs | verbatim copy only | selects the policy |

## Design Decision

Use a **Fleet control plane** in front of the existing session/run API and the
P82 primitives.

The model-visible surface is small and semantic. v1 ships four tools:

```text
agent_spawn     create a child by cloning/forking a source and start its run
agent_list      list related/child sessions with compact status
agent_read      read one session's status and config summary (no transcript)
agent_cancel    cancel an active run or close a child
```

Deferred to later passes (kept out of v1 to keep the surface tight):

```text
agent_configure   semantic config patch compiled into session/tool/env/mcp ops
agent_task        deliver follow-up work and start a run on an existing agent
agent_capabilities once policy is real; a static description suffices in v1
```

The internal implementation may call the existing `AgentApiService`, the P82
store methods, and the Temporal client directly. It should not bounce through
HTTP JSON-RPC when running in the same process.

Default subagents are **top-level Lightspeed sessions**, not Temporal Child
Workflows. Temporal remains an implementation substrate behind the API/runtime
boundary. A child agent gets its own workflow id, session log, config revision,
run ids, and inspectable status.

## Runtime Shape

```text
parent model tool call
  -> CoreAgent emits tool invocation
  -> Temporal tool activity runs outside deterministic engine
  -> Fleet service validates the request
  -> Fleet service computes fork cut-point (if fork) via P82 helper
  -> Fleet service clones/forks the source via P82 store methods
  -> Fleet service writes the parent->child link
  -> Fleet service calls internal AgentApiService / Temporal gateway logic
  -> child AgentSessionWorkflow starts as an ordinary session
  -> child run is admitted through run/start semantics
  -> parent tool result returns child handle
```

Starting another top-level workflow is a side effect. It belongs in an activity
or runtime service, not in `engine` and not in pure workflow reducer code.

The first hosted implementation should live in `temporal-server`. No new registry
crate is introduced for v1; revisit one only if `agent_id` ever diverges from
`session_id`.

## Identity Model

For P83, a Fleet agent is exactly a Lightspeed session. There is no separate
agent node, agent type, or role namespace in v1.

```text
agent_id == session_id
```

- `session_id`: event-sourced session/workflow identity, also the Fleet handle.
- `run_id`: one admitted execution inside a session.

The session log is the source of truth for transcript, durable events, config
revision, and run state. Product typing (`agent_type`, `role`, personas) is not
needed to prove the spawn loop and is deferred until there is product pressure
for it.

## Tool Contracts

### `agent_spawn`

Creates a child agent and optionally starts its first run.

Input shape:

```text
task_name
input
source?                 self | none | <session_id>   (default: self)
fork?                   bool                          (default: false)
fork_at_seq?            int                           (default: auto safe cut)
vfs?                    share | isolate  (default: share, isolate deferred)
environment?            share | isolate  (default: share, isolate deferred)
config_overrides?
lifecycle?
```

Defaults:

- `source`: the session the child is cloned/forked from. `self` (the common case)
  uses the caller; `none` starts a blank child; `<session_id>` uses another
  session the caller may access — see "Who can clone what" below. This enables
  "agent A spawns B, then clones B into C and D": A names B as the source. The
  inherited setup (config, MCP links, all log-borne state) comes from the
  *source's* live state, not the caller's.
- `fork`: when false (default), clone semantics (P82 clone — fresh log, config
  copied). `true` selects history fork (P82 fork — events inherited by reference
  to the branch point). The branch point defaults to the P82 safe cut (largest
  seq not inside an open run), so a fork triggered mid-spawn never inherits a
  dangling tool call. An explicit `fork_at_seq` overrides it but is rejected if
  it lands inside an open run.
- `vfs` / `environment`: `share` — the two per-session side-table resources are
  copied from the source verbatim (P82 default). They are independent knobs: a
  child may share the workspace but isolate the environment, or vice versa.
  `isolate` is reserved (see Resource Sharing).
- `lifecycle.run_immediately`: true.

Output:

```text
child_session_id
child_run_id?
status
```

`submission_id` is derived from the parent session/run/tool-call identity so
activity retries do not create duplicate children.

### Who can clone what

The P82 clone/fork primitive takes any source session id; nothing requires the
source to be the caller. The Fleet service decides who may name which source. An
agent may clone or fork:

- **itself** (`source = self`), the default;
- **a child it spawned** (provable from the spawn link);
- **any session it has a `session_link` to** — this is the access edge the links
  table exists for, and it is what lets A clone a peer B that A did not spawn.

In v1, `session_links` is unenforced data (per P82), so the spawn service
effectively trusts the named `source` id and records the lineage. Turning the
access edge into a hard check (caller must be self, spawner, or linked to the
source) is part of the deferred capability-policy work; the rule above is the
intended semantics.

### `agent_read` / `agent_list`

Read Fleet-level status and compact session/run projections.

These tools should not dump full transcripts by default. The caller must request
specific fields such as status, config summary, active run, children, or recent
events.

### `agent_cancel`

Cancels an active run or closes a child agent, depending on scope and policy.

Input shape:

```text
target_agent_id
scope                   active_run | queued_runs | session
reason?
```

## Resource Sharing: `vfs` and `environment`

P82's clone primitive copies the full set of `vfs_mounts` and
`session_environment_bindings` rows verbatim. P83 exposes whether that copy
**shares** the underlying mutable resource or **isolates** it, as two independent
spawn knobs (a child may share the workspace but run in its own environment, or
vice versa; collapsing them into one flag would make the uncommon case
unexpressible).

Each axis applies uniformly across the whole set of that session's rows.

v1 defaults both to **`share`** (the verbatim copy — child points at the same
mutable resource as the source):

- `vfs = share`: for every mount, child points at the same `workspace_id` head
  (or the same snapshot digest).
- `environment = share`: for every binding, child points at the same live
  `target_id`.

`isolate` is reserved per axis but its implementation is deferred:

- `vfs = isolate`: for each mount, fork `workspace`-kind mounts to a new
  `workspace_id` off the same base. `snapshot`-kind mounts are immutable CAS
  refs — sharing and isolating are identical, so they are always copied as-is.
- `environment = isolate`: for each binding, request a fresh target from *that
  binding's* provider, so the child gets its own parallel set of environments.

The shared default is fine for helper agents sharing context; `isolate` is the
right choice for independent workers, which is why the knobs exist from day one
even though the behavior lands later.

## Configuration Model

A spawned child **starts from the source's full setup, then patches** (the P82
inherit-then-patch rule). Child configuration is compiled as:

1. The source session's current `SessionConfig` and MCP links, inherited via the
   log (P82 clone/fork mechanics).
2. The source session's `vfs_mounts` and `session_environment_bindings`, copied
   per the `vfs` / `environment` knobs above.
3. Explicit `config_overrides` from the spawn request, applied as a patch *after*
   open — never as a rewrite of inherited state.

This deliberately uses the source's *live* config, not its session-start config.
Secrets and resolved credentials are never copied or stored on Fleet metadata
(P82: a clone inherits only the `grant_id` / `provider_id` reference and mints
its own token at call time).

Beta auth caveat: grants are `principal_kind = 'universe_default'`, so a clone
inherits the source's full auth reach by construction. There is no per-session or
per-grant scoping yet, so "spawn a less-privileged child" is **not expressible**
in v1 — it needs the `user` / `service_account` principal machinery plus
per-grant selection, part of the deferred capability-policy work.

Named profiles (tool/instructions/environment profiles, reusable run defaults)
are a later refinement, not part of v1.

## Policy

Capability-based policy is deferred. v1 gates the Fleet tools behind a single
per-session "may control fleet" flag and treats `session_links` as plain,
unenforced data. The richer capability matrix (per-operation grants, proposals
requiring approval, target-relation checks) is future work once there is product
pressure for it.

## Reentrancy

Do not expose a generic "call the session API" tool to the model.

When an agent spawns another agent, the tool call becomes a Fleet intent handled
by the runtime. The runtime admits any resulting session commands through normal
session/run boundaries. (Self-configuration arrives with the deferred
`agent_configure` tool; the same rule applies — changes take effect at the next
safe boundary, never by mutating the running model step in place.)

## Temporal Behavior

Default child creation:

```text
start_workflow(AgentSessionWorkflow, workflow_id = child_session_id)
signal_submit_admission(child_session_id, RequestRun)
```

Use an internal service or activity with a Temporal client. Do not make the
parent workflow's deterministic loop own external workflow creation directly.

Temporal Child Workflows are deferred. They are appropriate for bounded, attached
helper work where parent close/cancel behavior should govern the child. Fleet
subagents are durable product resources, so they should be top-level session
workflows by default.

## Implementation Steps

Assumes P82 store methods (`create_cloned_session`, `create_forked_session`, the
fork cut-point helper, fork read resolution, link CRUD) are available.

### G1. Fleet Service

- Add a runtime service that validates the request, resolves the `source` session
  (self, a spawned child, or a linked session — trusted by id in v1), reserves a
  child id, records the parent→child link and the child's `source_session_id`
  (+ `source_seq` when `fork = true`) lineage, clones or forks the source via the
  P82 store methods, and admits the child run.
- For `fork = true`, obtain the safe `source_seq` from the P82 cut-point helper;
  validate any explicit `fork_at_seq` is not inside an open run.
- Make spawn idempotent by a service-layer `submission_id`, reusing the existing
  run-admission idempotency pattern.
- Use internal `AgentApiService` calls, not HTTP loopback, when in the same
  process.

### G2. Model-Visible Tools

- Add the small Fleet tool package: `agent_spawn`, `agent_list`, `agent_read`,
  `agent_cancel`.
- Gate the tools behind a single per-session "may control fleet" flag.
- Keep schemas tight and deny unknown fields.

### G3. Child Session Configuration

- Compile the child's opening config from the source's live config plus explicit
  spawn overrides applied as a patch after open. The source may be the caller or
  any session the caller may use; the child inherits the source's setup.
- Apply the `vfs` / `environment` knobs over the P82 verbatim copy (v1: both
  `share`).
- Do not store secrets or resolved credentials.

### G4. Projection And Inspection

- Project `sessions` (incl. P82 lineage) and `session_links` into `agent_read`
  and `agent_list`, with compact child run status.
- Do not require full transcript reads for normal parent status checks.

### G5. Tests

- Unit-test validation, idempotency, capability checks, and config compilation.
- In-process runner test: a parent spawns a clone child and receives a handle.
- In-process runner test: a parent spawns a child cloned from a *different* named
  source (the "A clones C from B" flow).
- Fork-via-spawn test: forking from inside the spawning tool call yields a
  provider-valid child (the in-flight spawning run is not inherited, so the
  child's first turn has no dangling tool call).
- Ignored Temporal/Postgres live test proving a parent tool call starts a
  separate child `AgentSessionWorkflow` and child run.

## Deferred

- `agent_configure` (semantic self/child config patches) and `agent_task`
  (follow-up tasking of existing agents).
- `vfs = isolate` / `environment = isolate` behavior.
- `agent_type` / `role` / persona typing and named profiles.
- Capability-based policy, proposals-requiring-approval, and enforcement of
  `session_links`.
- Per-grant / less-privileged child auth scoping.
- Completion and important-update notifications back to the parent; rich
  `wait_agent` semantics.
- Temporal Child Workflow execution mode.
- Raw session API tools for privileged debugging.
- Sanitized forks (owned by P82's deferred list).

## Acceptance Criteria

- A parent agent can spawn a child cloned from itself, with a task, and receive a
  durable child handle (`child_session_id`).
- A parent agent can spawn a child cloned from a *different* source session it
  names, with the child's `source_session_id` recording that source.
- A parent agent can fork a source via spawn, and a fork triggered from inside the
  spawning tool call yields a provider-valid child (no dangling tool call).
- Retrying the same spawn tool call does not create a duplicate child.
- The child is visible through normal session/read behavior and through
  Fleet-level `agent_read`.
- The parent can cancel the child.
- The model-visible tool surface stays small (4 tools) and does not expose the
  full session API.
- No Fleet side effects are performed inside `engine`.
