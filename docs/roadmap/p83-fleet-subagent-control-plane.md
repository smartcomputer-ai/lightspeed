# P83: Fleet Subagent Control Plane

**Status**
- Proposed 2026-06-23.
- Completed 2026-06-23: G1-G7 are implemented (contracts/config gate,
  model-visible Fleet specs, spawn/task service, child config/resource policies,
  hosted tool routing, projection/inspection/cancel behavior, deterministic
  tests, and an ignored Temporal/Postgres live smoke).
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
2. Lightspeed validates policy and derives or validates a child identity.
3. Lightspeed creates the child as an ordinary top-level session backed by its
   own `AgentSessionWorkflow`, cloning or forking the source via the P82
   primitives and linking it to the caller in the Fleet graph.
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

The model-visible surface is small and semantic. v1 ships five tools:

```text
agent_spawn     create a child by cloning/forking a source and start its run
agent_task      deliver follow-up work and start a run on an existing agent
agent_list      list related/child sessions with compact status
agent_read      read one session's status, config, resources, and recent activity
agent_cancel    cancel an active run or close a child
```

Deferred to later passes (kept out of v1 to keep the surface tight):

```text
agent_configure   semantic config patch compiled into session/tool/env/mcp ops
agent_capabilities once policy is real; a static description suffices in v1
```

The internal implementation may call the existing `AgentApiService`, the P82
store methods, and the Temporal client directly. It should not bounce through
HTTP JSON-RPC when running in the same process.

Default subagents are **top-level Lightspeed sessions**, not Temporal Child
Workflows. Temporal remains an implementation substrate behind the API/runtime
boundary. A child agent gets its own workflow id, session log, config revision,
run ids, and inspectable status.

In Fleet terminology, "child" is a session-graph relationship, not a Temporal
workflow topology. A v1 `agent_spawn` always records a caller -> child link, even
though the spawned agent is an ordinary top-level session/workflow. Unlinked
standalone sessions remain an external/session API concern, not a model-visible
Fleet tool behavior.

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

Spawn identity is `child_session_id`, not a model-facing task label. The caller
may provide `child_session_id` when it needs a stable durable handle. If omitted,
the runtime derives one deterministically from the parent tool invocation
identity:

```text
parent_session_id + parent_run_id + turn_id + tool_batch_id + tool_call_id
```

The derived value is hashed/encoded into a valid `SessionId`. This makes tool
activity retries safe without relying on model-generated labels. Human display
names, task labels, and roles are metadata concerns and deferred until there is a
durable metadata model for them.

## Tool Contracts

### `agent_spawn`

Creates a child agent and optionally starts its first run.

Input shape:

```text
child_session_id?       explicit durable child session id
input
source?                 tagged source enum            (default: { kind: self })
fork?                   bool                          (default: false)
fork_at_seq?            int                           (default: auto safe cut)
vfs?                    share | isolate               (default: share)
environment?            share                         (default: share)
lifecycle?
```

Defaults:

- `child_session_id`: optional explicit durable handle. If absent, the runtime
  derives a deterministic id from the parent session/run/turn/tool-call identity.
  A retry with the same tool call therefore targets the same child session.
- `source`: the session the child is cloned/forked from. This must be a tagged
  enum, never a string sentinel, so a real session id named `self` is not
  ambiguous:

  ```json
  { "kind": "self" }
  { "kind": "session", "session_id": "self" }
  ```

  `{ "kind": "self" }` (the common case) uses the caller. `{ "kind": "session",
  "session_id": "..." }` uses another session the caller may access — see "Who
  can clone what" below. This enables "agent A spawns B, then clones B into C
  and D": A names B as the source. The inherited setup (config, MCP links, all
  log-borne state) comes from the *source's* live state, not the caller's.
  Blank/no-source or profile/template-based children are deferred until there is
  a concrete default-config and resource-setup story. When they land, they still
  create linked Fleet children; they just do not derive their setup from the
  caller/source session.
- `fork`: when false (default), clone semantics (P82 clone — fresh log, config
  copied). `true` selects history fork (P82 fork — events inherited by reference
  to the branch point). The branch point defaults to the P82 safe cut (largest
  seq not inside an open run), so a fork triggered mid-spawn never inherits a
  dangling tool call. An explicit `fork_at_seq` overrides it but is rejected if
  it lands inside an open run.
- `vfs`: `share` copies mounts verbatim. `isolate` gives the child its own
  writable VFS workspaces while preserving immutable snapshot mounts.
- `environment`: v1 accepts only `share`, so environment bindings are copied
  verbatim and point at the same live targets. Environment isolation is deferred.
- `lifecycle.run_immediately`: true.

Output:

```text
child_session_id
child_run_id?
status
```

Spawn idempotency is anchored by the child session id. When `child_session_id` is
omitted, the derived id is stable for the parent tool call. When it is supplied,
the service verifies an existing child with that id belongs to the same spawn
request before returning it. The child run `submission_id` is also derived from
the parent session/run/tool-call identity so run admission retries return the
same child run instead of starting a second one.

### Who can clone what

The P82 clone/fork primitive takes any source session id; nothing requires the
source to be the caller. The Fleet service decides who may name which source. An
agent may clone or fork:

- **itself** (`source = { "kind": "self" }`), the default;
- **a child it spawned** (provable from the spawn link);
- **any session it has a `session_link` to** — this is the access edge the links
  table exists for, and it is what lets A clone a peer B that A did not spawn.

In v1, `session_links` is unenforced data (per P82), so the spawn service
effectively trusts the named `source` id and records the lineage. Turning the
access edge into a hard check (caller must be self, spawner, or linked to the
source) is part of the deferred capability-policy work; the rule above is the
intended semantics.

Every v1 `agent_spawn` creates or reuses a parent -> child link from the caller
to the spawned session. The Fleet tool does not create unlinked standalone
sessions. If a human client or gateway needs a truly standalone session, it uses
the existing session API outside this model-visible Fleet surface.

### `agent_task`

Starts follow-up work on an existing Fleet agent session.

Input shape:

```text
target_agent_id
input
```

The target must already be a session. v1 trusts the named target id in the same
way `agent_read` and `agent_cancel` do; capability-policy enforcement is
deferred. The service uses the normal hosted `run/start` path with a
deterministic submission id derived from the parent tool invocation and target
agent id, so tool-activity retries do not admit duplicate runs.

### `agent_read` / `agent_list`

Read Fleet-level status and compact session/run projections. `agent_list` stays
compact by default. `agent_read` is the inspection tool for one agent and should
return enough state for a supervisor to understand what the child is, what it can
do, and what it is currently doing.

`agent_read` returns these fields by default:

- lifecycle/status;
- active run summary and recent completed run summaries;
- full effective `SessionConfig`;
- active tool names and default targets;
- VFS mounts and environment bindings;
- lineage/source fields and direct session links.

Transcript/activity is available but bounded. Full transcript dumps are not a
default because they can grow without limit and crowd out the useful control
state. The caller can request a recent window, for example:

```text
recent_transcript?      { turns?: int, events?: int }
recent_events?          { limit: int }
```

The default `agent_read` should include a small recent activity window when it is
cheap and bounded, enough to answer "what is going on?" without requiring a
separate transcript read. Larger history reads remain explicit and paged.

### `agent_cancel`

Cancels an active run or closes a child agent, depending on scope and policy.

Input shape:

```text
target_agent_id
scope                   active_run | session
reason?
```

v1 deliberately does not advertise queued-run cancellation because the engine
does not yet expose a safe queued-admission removal command. The supported
control operations are active-run cancellation and session close.

## Resource Sharing: `vfs` and `environment`

P82's clone primitive copies the full set of `vfs_mounts` and
`session_environment_bindings` rows verbatim. P83 exposes the selected resource
policy on each axis. v1 implements `share` and `isolate` for VFS, but implements
only `share` for environments because creating fresh equivalent environment
targets is provider-specific lifecycle work.

Each axis applies uniformly across the whole set of that session's rows.

v1 defaults both axes to **`share`**:

- `vfs = share`: for every mount, child points at the same `workspace_id` head
  (or the same snapshot digest).
- `environment = share`: for every binding, child points at the same live
  `target_id`.

v1 also implements **`vfs = isolate`**:

- For each `workspace`-kind mount, create a deterministic child `workspace_id`
  from `child_session_id + mount_path`, read the source workspace head, and
  create the child workspace with that head as its starting snapshot. The child
  mount is then rewritten to point at the child workspace. Future child writes
  advance only the child workspace head.
- `snapshot`-kind mounts are immutable CAS refs. Sharing and isolating are
  equivalent for them, so they are copied as-is.
- The rewrite must be idempotent so a retry after child-session creation but
  before run admission can safely finish applying the policy.

`environment = isolate` is not accepted by the v1 schema. It is deferred until
environment providers expose a way to request fresh equivalent targets.

## Configuration Model

A spawned child **inherits the source's full setup**. Child configuration is
compiled as:

1. The source session's current `SessionConfig` and MCP links, inherited via the
   log (P82 clone/fork mechanics).
2. The source session's `vfs_mounts` and `session_environment_bindings`, copied
   per the policies above (`vfs = share|isolate`, `environment = share` only).

This deliberately uses the source's *live* config, not its session-start config.
Secrets and resolved credentials are never copied or stored on Fleet metadata
(P82: a clone inherits only the `grant_id` / `provider_id` reference and mints
its own token at call time).

`agent_spawn` does not accept ad hoc config patches in v1. Raw API config patch
syntax is intentionally kept out of model-visible Fleet tools to avoid mixing
tool snake_case with API camelCase and to avoid a second patch dialect. Follow-up
configuration belongs in a future `agent_configure` tool or profile-based
creation flow.

Beta auth caveat: grants are `principal_kind = 'universe_default'`, so a clone
inherits the source's full auth reach by construction. There is no per-session or
per-grant scoping yet, so "spawn a less-privileged child" is **not expressible**
in v1 — it needs the `user` / `service_account` principal machinery plus
per-grant selection, part of the deferred capability-policy work.

Named profiles (tool/instructions/environment profiles, reusable run defaults)
are a later refinement, not part of v1.

## Policy

Capability-based policy is deferred. v1 gates the Fleet tools behind a single
per-session "may control fleet" flag, represented in session tool config (for
example `tools.fleet = true`), and treats `session_links` as plain, unenforced
data. Adding this flag means updating the engine `ToolConfig`, API config
input/patch/view types, API projection, gateway config conversion, and committed
contract artifacts. The richer capability matrix (per-operation grants,
proposals requiring approval, target-relation checks) is future work once there
is product pressure for it.

## Reentrancy

Do not expose a generic "call the session API" tool to the model.

When an agent spawns another agent, the tool call becomes a Fleet intent handled
by the runtime outside the deterministic engine. The parent session records only
the normal tool-call result. Child session creation, resource-policy application,
workflow start, and child run admission are side effects owned by the hosted
runtime/tool activity.

Rules:

- `agent_spawn` must never mutate the currently running parent step in place.
- A spawn from `source = { "kind": "self" }` is allowed while the parent has an
  active run. If `fork = true`, P82's safe cut excludes the in-flight parent run,
  including the spawning tool call, so the child never inherits dangling tool
  state.
- `agent_spawn` must not use itself as a generic backdoor to call arbitrary
  `session/*` or `run/*` operations on the parent. Self-configuration is deferred
  to `agent_configure` and applies only at a safe boundary.
- Idempotency is required across tool activity retries. Reusing the deterministic
  child id must return the existing child only when the recorded source/link
  metadata matches the same spawn request; a collision with unrelated metadata is
  rejected.

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

## Implementation Map

- Tool contracts/spec bundles live in `crates/tools/src/fleet/`. This crate should define the model-visible JSON
  schemas, strict argument DTOs, output DTOs, tool names, and `ToolSpecBundle`
  helpers. It should not depend on PostgreSQL or Temporal.
- Toolset exposure is wired through `crates/tools/src/toolset.rs` with a
  `FleetToolsetConfig`. The gateway's session-toolset assembly enables it from
  the per-session config gate.
- Hosted execution lives in `temporal-server`, for example
  `crates/temporal-server/src/fleet.rs` plus worker wiring. `FleetService` owns
  the runtime behavior: source resolution, deterministic child id derivation,
  P82 clone/fork calls, VFS policy application, link upsert, workflow start, and
  child run admission.
- `SessionTools` in `crates/temporal-server/src/worker/session_tools.rs` routes
  `agent_*` calls to the Fleet executor directly, similar to the existing
  messaging fast path. The generic `InlineToolRuntime` currently executes only
  builtins/web-fetch style bindings, so exposing Fleet specs in the toolset is
  not enough by itself.
- Clone/fork opening config uses the source session's replayed CoreAgent state
  and `core_agent_clone_opening_events`. Store-level clone/fork/link operations
  stay the P82 primitives.
- VFS isolation uses the existing `VfsWorkspaceStore` and `VfsMountStore` APIs:
  read source mounts/workspaces, create deterministic child workspaces from
  source heads, and rewrite child mounts before child run admission.
- `agent_read` / `agent_list` reuse gateway projection helpers and P82
  `session_links` queries. `agent_read` returns full effective config plus
  compact resources, lineage, status, and bounded recent transcript/activity.
  `agent_list` stays compact.

## Implementation Steps

Assumes P82 store methods (`create_cloned_session`, `create_forked_session`, the
fork cut-point helper, fork read resolution, link CRUD) are available.

### G1. Contracts And Config Gate — Done 2026-06-23

- Finalize the v1 tool DTOs: `child_session_id?`, `input`, tagged `source`,
  `fork`, `fork_at_seq`, `vfs`, `environment`, `lifecycle`.
- Add the per-session Fleet tool gate to engine/API config and regenerate
  committed API contract artifacts.
- Add strict schemas that deny unknown fields and do not advertise deferred
  values (`environment = isolate`, blank source, display labels).

Implementation note: an earlier draft accepted raw `config_overrides`, but the
completed v1 surface removed it. `agent_spawn` inherits source config only;
configuration changes are deferred to `agent_configure` or profile-based
creation.

### G2. Model-Visible Tools — Done 2026-06-23

- Add the small Fleet tool package: `agent_spawn`, `agent_task`, `agent_list`,
  `agent_read`, `agent_cancel`.
- Wire Fleet specs into `ToolsetConfig` / `resolve_toolset` only when the
  per-session Fleet gate is enabled.
- Keep the surface small; do not expose generic session/run/VFS/environment APIs
  to the model.

Implementation note: tool specs are exposed only when `tools.fleet = true`, and
hosted `SessionTools` routes Fleet calls to the Fleet executor when the worker is
constructed with the Postgres/Temporal runtime.

### G3. Fleet Service — Done 2026-06-23

- Add a runtime service that validates the request, resolves the `source` session
  (self, a spawned child, or a linked session — trusted by id in v1), derives or
  validates the child id, records the parent->child link and the child's
  `source_session_id` (+ `source_seq` when `fork = true`) lineage, clones or
  forks the source via the P82 store methods, and admits the child run. Also
  support follow-up `agent_task` run admission on existing target sessions.
- For `fork = true`, obtain the safe `source_seq` from the P82 cut-point helper;
  validate any explicit `fork_at_seq` is not inside an open run.
- Make spawn idempotent by deterministic child id plus link metadata validation.
  Reuse the existing run-admission `submission_id` pattern for the child run.
- Use internal `AgentApiService` calls, not HTTP loopback, when in the same
  process.

Implementation note: the current service supports clone and fork spawn, safe fork
cut-point selection, explicit child ids, deterministic derived child ids,
parent->child link metadata validation, child workflow/session start, optional
immediate child run admission, and follow-up `agent_task` run admission with
deterministic submission ids. `vfs = share` and `environment = share` are
supported through P82's verbatim resource copy; `vfs = isolate` is handled by the
G4 resource-policy pass.

### G4. Child Session Configuration And Resources — Done 2026-06-23

- Compile the child's opening config from the source's live config. The source
  may be the caller or any session the caller may use; the child inherits the
  source's setup.
- Apply resource policies after P82's verbatim resource copy and before child run
  admission: `vfs = share|isolate`, `environment = share`.
- Reject unsupported v1 policy values clearly.
- Do not store secrets or resolved credentials.

Implementation note: `vfs = isolate` rewrites copied workspace mounts to
deterministic child workspaces based on `child_session_id + mount_path`; snapshot
mounts stay shared. `environment = share` remains the only accepted environment
policy. Ordinary spawn retries with matching Fleet link metadata skip the pre-run
setup pass and reuse the deterministic child run submission id.

### G5. Hosted Runtime Wiring — Done 2026-06-23

- Wire `SessionTools` to detect Fleet tool names and route them to a Fleet tool
  executor with the parent `session_id`, `run_id`, `turn_id`, `batch_id`, and
  `call_id`.
- Ensure Fleet tool execution uses the same blob/result shape as other tools and
  returns compact model-visible handles/status.
- Keep all Fleet side effects out of `engine` and deterministic workflow reducer
  code.

Implementation note: hosted workers inject a Fleet executor into `SessionTools`
when constructed from the Postgres/Temporal runtime. `agent_spawn`, `agent_task`,
`agent_list`, `agent_read`, and `agent_cancel` route to the Fleet executor with
normal tool-result blobs and model-visible summaries.

### G6. Projection And Inspection — Done 2026-06-23

- Project `sessions` (incl. P82 lineage) and `session_links` into `agent_read`
  and `agent_list`, with compact child run status.
- Make `agent_read` return full effective config by default, not just a summary.
- Include resource summaries: active tools/default targets, VFS mounts, and
  environment bindings.
- Support bounded recent transcript/activity selectors so parent agents can
  inspect what a child is doing without reading the entire log.
- Keep full-history transcript reads explicit and paged.

Implementation note: `agent_read` wraps the normal `session/read` projection
(including full effective config, runs, active tools, context, and VFS mounts)
and adds P82 lineage, direct links, environment bindings, bounded recent event
windows, and a bounded recent transcript window. `agent_list` uses Fleet
parent/child links and returns compact status/active-run fields. `agent_cancel`
uses the hosted run/session API for active-run cancellation and session close.

### G7. Tests — Done 2026-06-23

- Unit-test validation, idempotency, capability checks, strict schema behavior,
  deterministic child-id derivation, and config compilation.
- In-process runner test: a parent spawns a clone child and receives a handle.
- In-process runner test: a parent spawns a child cloned from a *different* named
  source (the "A clones C from B" flow).
- Fork-via-spawn test: forking from inside the spawning tool call yields a
  provider-valid child (the in-flight spawning run is not inherited, so the
  child's first turn has no dangling tool call).
- VFS policy tests: `share` keeps workspace ids, `isolate` creates deterministic
  child workspaces from source heads, snapshot mounts remain unchanged, and retry
  is idempotent.
- Environment policy test: `share` copies bindings; unsupported isolation is
  rejected by schema/validation.
- Projection tests: `agent_read` returns full effective config, resource
  summaries, lineage, and bounded recent transcript/activity; `agent_list`
  remains compact.
- Ignored Temporal/Postgres live test proving a parent tool call starts a
  separate child `AgentSessionWorkflow` and child run.

Implementation note: deterministic coverage now includes strict Fleet schemas,
spawn idempotency, explicit-id collision handling, VFS isolation, hosted
`SessionTools` routing, `agent_task`, `agent_read`, `agent_list`, and
`agent_cancel`. The ignored live smoke
`temporal_live_fleet_executor_spawns_child_workflow_and_run` exercises the real
Fleet executor against the Postgres/Temporal runtime and verifies the child
workflow plus initial and follow-up child runs complete.

## Deferred

- `agent_configure` (semantic self/child config patches).
- Blank/no-source and profile/template-based child creation.
- Human labels/display names/task names.
- Unlinked standalone session creation from inside Fleet tools.
- `environment = isolate` behavior.
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
- `child_session_id` may be supplied explicitly; otherwise it is derived
  deterministically from the parent tool invocation identity.
- `source` is a tagged enum, so a real session id named `self` is unambiguous.
- VFS `share` and `isolate` are implemented; environment sharing is implemented
  and environment isolation is not accepted by v1 schemas.
- Every v1 spawn records a caller -> child session link; unlinked standalone
  session creation is not exposed through `agent_spawn`.
- The parent can task an existing child/agent with follow-up work and receives
  the admitted run handle.
- The child is visible through normal session/read behavior and through
  Fleet-level `agent_read`.
- `agent_read` returns full effective config and supports bounded recent
  transcript/activity inspection.
- The parent can cancel a child's active run or close the child session.
- The model-visible tool surface stays small (4 tools) and does not expose the
  full session API.
- No Fleet side effects are performed inside `engine`.
