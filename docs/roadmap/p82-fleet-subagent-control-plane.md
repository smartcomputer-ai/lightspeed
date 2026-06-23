# P82: Fleet Subagent Control Plane

**Status**
- Proposed 2026-06-23.
- Builds on the current `api` session/run surface, the Temporal-backed
  `AgentSessionWorkflow`, environment/provider work from P79-P81, and
  `docs/spec/05-subagents-reference-study.md`.
- This is the first concrete Fleet/subagent implementation plan.

## Goal

Let an agent safely create, configure, task, inspect, and cancel other agents.

The first implementation should support the happy path:

1. A parent agent calls a small Fleet tool such as `agent_spawn`.
2. Lightspeed validates policy and reserves a child identity.
3. Lightspeed creates the child as an ordinary session backed by its own
   `AgentSessionWorkflow`.
4. Lightspeed applies the child run configuration.
5. Lightspeed starts the child run with the requested task.
6. The parent receives a durable child handle it can inspect or task later.

The model should not receive the raw `session/*`, `run/*`, environment, MCP,
auth, VFS, and blob APIs as separate tools. It should receive a small Fleet
control surface that compiles intent into those lower-level API operations.

## Design Decision

Use a **Fleet control plane** in front of the existing session/run API.

The model-visible surface is small and semantic. v1 ships four tools:

```text
agent_spawn     create a child by cloning the parent and start its first run
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

The internal implementation may call the existing `AgentApiService`, stores, and
Temporal client directly. It should not bounce through HTTP JSON-RPC when running
in the same process.

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
  -> Fleet service writes/updates Fleet metadata
  -> Fleet service calls internal AgentApiService / Temporal gateway logic
  -> child AgentSessionWorkflow starts as an ordinary session
  -> child run is admitted through run/start semantics
  -> parent tool result returns child handle
```

Starting another top-level workflow is a side effect. It belongs in an activity
or runtime service, not in `engine` and not in pure workflow reducer code.

The first hosted implementation should live in `temporal-server`, with the
clone/fork primitive and link CRUD added to the existing `SessionStore` trait
and `store-pg`. No new registry crate is introduced for v1; revisit one only if
`agent_id` ever diverges from `session_id`.

## Identity Model

For P82, a Fleet agent is exactly a Lightspeed session. There is no separate
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

## Data Model

P82 adds no new durable record types and no registry crate. It extends the
existing `sessions` table and adds one relationships table in
`crates/store-pg/migrations/001_core.sql`.

### Clone / Fork Lineage (on `sessions`)

Two nullable columns record where a session's content came from:

```text
source_session_id   content origin; NULL for a fresh root session
source_seq          NULL  -> config-only clone, child log starts at seq 1
                    set   -> history fork, child log continues at source_seq + 1
```

Both clone and fork ship in v1:

- **Clone** (the common case): create a new session whose opening config is
  built from the source session's *live* config (model, tools, run defaults),
  plus copied MCP links and environment bindings. Fresh event log at seq 1.
  `source_seq` is NULL.
- **Fork**: the parent's events are inherited **by reference, not copied**.
  `source_seq = N` marks the branch point; the child's own rows start at `N + 1`
  and the child's *effective* log is the parent's events `1..N` followed by the
  child's own rows. The child rehydrates `CoreAgentState` by replaying that
  stitched log. `N` is chosen so it never falls inside an open run (see
  "Context Forking → Fork point"), since a fork is taken mid-spawn.

`source_session_id` only records the content origin. It is independent of who
initiated the clone/fork: agent A may clone or fork agent C from agent B, in
which case the new session's source is B, regardless that A drove the call.

#### Fork by reference (copy-on-write history)

A fork stores a pointer, not a copy, so forks are O(1) and cheap to take in
quantity. Reading a forked session's log resolves the chain:

- The `source` chain can be arbitrarily deep — a fork may fork a fork. Reading
  walks it recursively: for `A[1..10] -> B[11..20] -> C[21..]`, C's effective
  log is `A[1..10] ++ B[11..20] ++ C[21..]`.
- Seqs stay a single **contiguous** line across the chain (child continues at
  `source_seq + 1`), so a requested window either falls inside one segment or
  spans a cut point; the store routes each sub-range to the physical session that
  owns it and concatenates. No seq remapping.
- Each upstream segment is **clamped to the child's `source_seq`**, never the
  parent's live head. A parent may keep appending after being forked (forks are
  allowed at any seq, and one parent may be forked many times at different
  points — forks form a tree); those later parent events belong to the parent's
  branch, not the fork's. Clamping is what makes a fork a branch rather than a
  shared tail.

This resolution lives in the **store/read layer** (`SessionStore::read_after` and
head resolution). The engine and workflow rehydrate from what looks like an
ordinary contiguous log and are unaware that a fork happened. The append path is
unchanged: `seq` is `head + 1` under optimistic concurrency, and the child's
head is seeded from `source_seq` so its first append lands at `source_seq + 1`.
Nothing hardcodes seq 1 (one engine test asserts it for fresh sessions only).

### Session Links (new table)

A directed, typed relationship between two sessions:

```text
from_session_id     "from can <relationship> to"
to_session_id
relationship        open string: can_see | can_configure | ... (grows freely)
created_at_ms
metadata
```

Purpose: record which sessions a given session may see, access, or configure.
In v1 this is plain data — nothing enforces it; Fleet tooling reads it when the
control surface lands. Links are set independently of clone/fork lineage and can
be created manually between any two existing sessions, including sessions that
never spawned each other (start session 1, start session 2, then relate them).

`relationship` is an open string so the vocabulary grows without migration, and
the direction lets access be asymmetric. No idempotency column lives here; spawn
idempotency, when spawn is built, is a service-layer `submission_id` check reusing
the existing run-admission pattern.

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

- `source`: the session the child is cloned from. `self` (the common case)
  clones the caller; `none` starts a blank child; `<session_id>` clones another
  session the caller may access — see "Who can clone what" below. This is what
  enables "agent A spawns B, then clones B into C and D": A names B as the
  source. The cloned setup (config, MCP links, all log-borne state) is taken from
  the *source's* live state, not the caller's.
- `fork`: when false (default), clone semantics — fresh log, config-only. `true`
  selects history fork: the source's events are inherited by reference up to the
  branch point and the child continues from there. The branch point defaults to
  the largest seq that is not inside an open run (see "Fork point" below), so a
  fork triggered mid-spawn never inherits a dangling tool call. An explicit
  `fork_at_seq` may override it but is rejected if it lands inside an open run.
- `vfs` / `environment`: `share` — the two per-session side-table resources are
  copied from the source verbatim. They are independent knobs: a child may share
  the workspace but isolate the environment, or vice versa. `isolate` is reserved
  (see Configuration Model).
- `lifecycle.run_immediately`: true.

### Who can clone what

The clone/fork primitive takes any source session id; nothing requires the source
to be the caller. An agent may clone or fork:

- **itself** (`source = self`), the default;
- **a child it spawned** (provable from `agent` lineage / the spawn link);
- **any session it has a `session_link` to** — this is the access edge the links
  table exists for, and it is what lets A clone a peer B that A did not spawn.

In v1, `session_links` is unenforced data, so the spawn service effectively trusts
the named `source` id and records the lineage. Turning the access edge into a hard
check (caller must be self, spawner, or linked to the source) is part of the
deferred capability-policy work; the rule above is the intended semantics, written
down now so the data model already supports it.

Output:

```text
child_session_id
child_run_id?
status
```

`submission_id` is derived from the parent session/run/tool-call identity so
activity retries do not create duplicate children.

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

## Configuration Model

A clone or fork **starts from the source's full setup, then patches** — never the
reverse. The child first inherits the source's complete config and resources,
and any modification is applied as a second step *after* replay, as a new
`ConfigChanged` event (or re-bind/re-mount), never as a rewrite of what was
inherited. This holds identically for clone and fork.

### Config lives in two places

Not all of a session's setup is in the event log. Cloning has to account for
both, and the two categories are cloned very differently.

**In the event log** (reconstructed by replay — no row copy needed):

- `SessionConfig`: model, run defaults, tools.
- MCP links — per `003_mcp.sql`, session-visible links are *materialized into
  tool-set events*, not stored per session in a side table. They ride the log.

These are inherited automatically when the child opens from the source's *live*
config (the fold of `Opened` + every `ConfigChanged`), not the session-start
config — the source may have changed model/tools/links during its lifetime.

MCP follows the same inherit-then-patch lifecycle as everything else: because the
links live in the log, the child inherits exactly the source's linked servers on
clone/fork, and any divergence (link a new server, unlink one) is a *later*
tool-set event appended after the open/fork point — never a rewrite of an
inherited link. The shared `mcp_servers` catalog itself is universe-scoped, so
the child can already resolve every server the source could; cloning only decides
which of them are linked, and that is carried by the log.

**In per-session side tables** (must be re-created for the child). Both are
**one-to-many per session** — a session attaches *many* environments (PK
`(session_id, env_id)`, switched between at call time) and mounts *many*
workspaces/snapshots (PK `(session_id, mount_path)`). Cloning copies the **whole
set**, not a single row:

| table | cardinality | clone action |
|---|---|---|
| `vfs_mounts` (002) | N per session | copy *all* mount rows |
| `session_environment_bindings` (006) | N per session | copy *all* binding rows |
| `mcp_servers`, `auth_*`, `environment_providers/targets`, `vfs_workspaces` | universe-scoped | nothing; child already sees them |

Auth is the cleanest case: no `auth_*` table carries `session_id`. What a session
references is a `grant_id` (MCP, carried in the log via `RemoteMcpToolSpec.auth_ref`)
or a `provider_id` (LLM keys, from deployment config; the default beta path falls
back to `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` env vars rather than `auth_secrets`).
Both resolve to a **live token at call time** via the universe broker — single-flight
refreshed, never written to the session log, request blobs, or Fleet metadata. A
clone therefore inherits only the *reference*, and mints its *own* token at its own
call time. There is nothing to copy and no token to leak or go stale.

Beta caveat: grants are `principal_kind = 'universe_default'`, so a clone inherits
the source's full auth reach by construction. There is no per-session or per-grant
scoping yet, so "spawn a less-privileged child" is **not expressible** in v1 — it
needs the `user` / `service_account` principal machinery plus per-grant selection,
which is part of the deferred capability-policy work.

### Resource sharing: `vfs` and `environment`, each `share | isolate`

The two per-session tables are independent isolation axes and get their own
spawn field — a child may share the workspace but run in its own environment, or
share the environment while editing a private workspace. They rarely differ, but
collapsing them into one flag would make the uncommon case unexpressible.

Each axis applies **uniformly across the whole set** of that session's rows.
Copying a row verbatim means the child points at the *same* mutable resource as
the source. v1 defaults both to **`share`** (verbatim copy of every row, simplest):

- `vfs = share`: for every mount, child points at the same `workspace_id` head
  (or the same snapshot digest).
- `environment = share`: for every binding, child points at the same live
  `target_id`.

`isolate` is reserved per axis but its implementation is deferred:

- `vfs = isolate`: for each mount, fork `workspace`-kind mounts to a new
  `workspace_id` off the same base. `snapshot`-kind mounts are immutable CAS
  refs — sharing and isolating are identical, so they are always copied as-is and
  isolation does no work on them.
- `environment = isolate`: for each binding, request a fresh target from *that
  binding's* provider, so the child gets its own parallel set of environments.

The shared default is fine for helper agents sharing context; `isolate` is the
right choice for independent workers, which is why the knobs exist from day one
even though the behavior lands later.

### Clone / fork × config

History and config are independent axes, but config-less fork is not a real
option: the log is config-bearing (the first event is `Opened{config}`), so
copying an event prefix inherently copies the config it asserts. A fork therefore
always inherits the source's config from its prefix; you cannot fork the
transcript while starting from a blank model.

- **clone**: `config_overrides` may be applied at spawn time (no inherited
  history to contradict) or later as a patch.
- **fork**: config is inherited from the prefix; changes are allowed only as new
  events *after* the fork point, never as a rewrite of the inherited prefix —
  rewriting it breaks replay coherence and cache compatibility (the same reason
  Codex rejects model/effort overrides on full-history forks).

Named profiles (tool/instructions/environment profiles, reusable run defaults)
are a later refinement, not part of v1.

## Context Forking

The child should not inherit the full source transcript by default. The default
is clone (`fork = false`: fresh log, config copied), which corresponds to a
`none` context policy plus an explicit task.

History fork (`fork = true`) is the opt-in mechanism for richer context
inheritance, and it ships in v1 via the by-reference resolution above — no event
copying, so it is cheap. It stays opt-in because it increases context size,
privacy exposure, and child confusion. Clone and fork share the same `source`
selection, so an agent can fork itself or any session it may access, just as it
can clone.

### Fork point: never cut inside a run

A fork is triggered from *inside* the spawning `agent_spawn` tool call, so the
parent's live head sits mid-turn: an assistant message has emitted the spawn
tool call but no tool result exists yet. Forking at the raw head would inherit a
dangling tool call with no result, which most providers reject on the child's
first turn. So choosing the cut point is the v1 "sanitization" — and because the
fork is by reference, it is done by picking `source_seq`, not by editing events.

The single invariant: **the cut may land anywhere except inside an open (not yet
terminal) run.** A run's interior is the only place a dangling tool call / open
turn can exist; everywhere else the log is replay-safe.

Default fork point:

- Include *everything* up to the start of the in-flight run — completed runs in
  full **and** the loose non-run events between/before them (`Opened`,
  `ConfigChanged`, context appends, compaction summaries). These are real
  inherited history and are not dropped.
- Exclude only the in-flight spawning run. Concretely, `source_seq` = the seq
  just before the active run's first event (`Run::Accepted`/`Started`). The
  runner knows `runs.active` when it handles the spawn call, so this is a state
  lookup, not a log scan.
- If there is no active run at all (forking a quiescent session), the cut is
  simply the head — nothing is open. Excluding the in-flight run is just the
  special case where one exists.

Fallbacks when nothing of this session precedes the open run:

- If only standalone events precede it (e.g. `Opened` + some `ConfigChanged`, no
  completed run yet), the fork still includes them — a short but genuine fork,
  not a degrade-to-clone.
- If *nothing* of this session precedes it, cut at the session's start: `0` for a
  root session, or the parent's own `source_seq` if this session was itself
  forked (pointing at the grandparent's branch point). This is ordinary chain
  resolution with an empty local segment — no special case in the read path,
  only in the recorded `source_seq`.

This generalizes the "fork from the last completed run" intuition: the boundary
is defined by *never cutting inside a run*, not by enumerating run terminals, so
non-run history is preserved and quiescent forks fall out naturally.

### Deferred fork refinements

Beyond choosing a safe cut point, fork inherits the prefix verbatim by reference.
A sanitized fork (drop incomplete tool calls, raw tool outputs, provider noise;
keep durable user/developer/system facts, final answers, compaction summaries —
per the
reference study) is a later refinement layered on the same chain resolution, not
a change to the storage model.

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

Temporal Child Workflows are deferred. They are appropriate for bounded,
attached helper work where parent close/cancel behavior should govern the child.
Fleet subagents are durable product resources, so they should be top-level
session workflows by default.

## Implementation Steps

### G1. Schema, Clone, And Links

- Add `source_session_id` / `source_seq` columns to `sessions` and the
  `session_links` table in `001_core.sql` (done).
- Extend `SessionStore` with `create_cloned_session(source, config_source)`,
  `create_forked_session(source, source_seq)`, and link CRUD
  (`upsert_link` / `list_links`).
- Implement clone in `store-pg`: copy the source session's live config, MCP
  links, and environment bindings into a fresh child session opened at seq 1.
- Implement fork creation in `store-pg`: write the child row with
  `source_session_id` + `source_seq`, seed its head from `source_seq` so the
  first append lands at `source_seq + 1`. No event copying.
- Unit-test clone (opens at seq 1 with the source's config), fork creation
  (`source_seq` round-trips, first append is `source_seq + 1`), and a manual
  link between two pre-existing sessions.

### G1b. Fork Read Resolution

- Implement chain-aware reads in `store-pg` behind `SessionStore`:
  `read_after` and `head` resolve the `source_session_id` chain recursively,
  stitching `parent[1..source_seq] ++ ... ++ self[source_seq+1..]` into one
  contiguous seq line, clamping each upstream segment to its child's `source_seq`.
- Keep this fully behind the store trait: engine/workflow rehydration is
  unchanged and unaware of forks.
- Unit-test multi-level forks (fork of a fork), a window spanning a cut point,
  and clamping (parent appends after the fork are invisible to the child).

### G2. Fleet Service

- Add a runtime service that validates the request, resolves the `source`
  session (self, a spawned child, or a linked session — trusted by id in v1),
  reserves a child id, records the parent→child link and the child's
  `source_session_id` (+ `source_seq` when `fork = true`) lineage, clones or
  forks the source session, and admits the child run.
- For `fork = true`, compute the safe `source_seq` from the source's run state:
  the seq just before the active run's first event, else the head; apply the
  empty-prefix fallbacks (standalone events kept; otherwise the session's start
  / its own `source_seq`). Validate any explicit `fork_at_seq` is not inside an
  open run.
- Make spawn idempotent by a service-layer `submission_id`, reusing the existing
  run-admission idempotency pattern.
- Use internal `AgentApiService` calls, not HTTP loopback, when in the same
  process.

### G3. Model-Visible Tools

- Add the small Fleet tool package: `agent_spawn`, `agent_list`, `agent_read`,
  `agent_cancel`.
- Gate the tools behind a single per-session "may control fleet" flag.
- Keep schemas tight and deny unknown fields.

### G4. Child Session Configuration

- Build the child's opening config from the *source* session's *live* config
  (model, tools, MCP links — all carried by the log) plus explicit spawn
  overrides applied as a patch after open. The source may be the caller or any
  session the caller may clone; the child inherits the source's setup, not the
  caller's.
- Copy per-session side-table rows: `vfs_mounts` and
  `session_environment_bindings`. For v1 both axes default to `share` (copy
  verbatim). The `vfs` and `environment` isolate paths (fork workspace, request
  fresh target) are deferred and independently selectable.
- Do nothing for universe-scoped catalogs (`mcp_servers`, `auth_*`, environment
  providers/targets): the same-universe child already resolves them.
- Do not store secrets or resolved credentials.

### G5. Projection And Inspection

- Project `sessions` (incl. lineage) and `session_links` into `agent_read` and
  `agent_list`, with compact child run status.
- Do not require full transcript reads for normal parent status checks.

### G6. Tests

- Unit-test validation, idempotency, capability checks, and config compilation.
- Add an in-process runner test where a parent spawns a child and receives a
  child handle.
- Add a fork test: a forked child's effective log stitches the source prefix
  with its own appends, multi-level forks resolve, and a post-fork parent append
  is invisible to the child (clamping).
- Add a fork-cut-point test: forking mid-spawn excludes the in-flight run (no
  dangling tool call inherited), standalone non-run events before a run are kept,
  the empty-prefix fallback cuts at session start / parent `source_seq`, and an
  explicit `fork_at_seq` inside an open run is rejected.
- Add an ignored Temporal/Postgres live test proving a parent tool call starts a
  separate child `AgentSessionWorkflow` and child run.

## Deferred

- `agent_configure` (semantic self/child config patches) and `agent_task`
  (follow-up tasking of existing agents).
- Sanitized forks (dropping incomplete tool calls / raw outputs / provider
  noise). Verbatim by-reference fork ships in v1; sanitization layers on the
  same chain resolution later.
- `agent_type` / `role` / persona typing and named profiles.
- Capability-based policy, proposals-requiring-approval, and enforcement of
  `session_links`.
- Completion and important-update notifications back to the parent; rich
  `wait_agent` semantics.
- Temporal Child Workflow execution mode.
- Raw session API tools for privileged debugging.

## Acceptance Criteria

- A parent agent can spawn a child cloned from itself, with a task, and receive
  a durable child handle (`child_session_id`).
- A parent agent can spawn a child cloned from a *different* source session it
  names (the "A clones C from B" flow), with the child's `source_session_id`
  recording B.
- A parent agent can fork a source session: the child's effective log reads the
  source's events up to the branch point (by reference, recursively for a fork of
  a fork) followed by its own, and a parent append after the fork does not appear
  in the child.
- A fork triggered from inside the spawning tool call yields a provider-valid
  child: the in-flight spawning run is not inherited, so the child's first turn
  has no dangling tool call.
- Retrying the same spawn tool call does not create a duplicate child.
- The child is visible through normal session/read behavior and through
  Fleet-level `agent_read`.
- Two sessions started independently can be put into a `session_link` and the
  link reads back.
- The parent can cancel the child.
- The model-visible tool surface stays small (4 tools) and does not expose the
  full session API.
- No Fleet side effects are performed inside `engine`.
