# P82: Session Graph — Clone, Fork, And Links

**Status**
- Proposed 2026-06-23.
- Completed 2026-06-23.
- Foundation layer. Builds on the current `api` session/run surface, the
  event-sourced session log, and the `store-pg` schema.
- Split out of the original Fleet plan: this doc is the **session-graph
  primitive** (store, schema, read algorithm) with no agent-facing surface. The
  model-visible control plane that consumes it is **P83 (Fleet Subagent Control
  Plane)**.

**Completion Notes**
- `SessionStore` now exposes clone, fork, safe fork-point, and session-link
  primitives. `SessionRecord` carries `source_session_id` / `source_seq`.
- `store-pg` persists lineage, copies `vfs_mounts` and
  `session_environment_bindings`, reads fork chains by reference, clamps parent
  tails, and stores directed `session_links`.
- `InMemorySessionStore` implements the same fork/link/read behavior for
  deterministic tests and in-process runner coverage.
- `CoreAgent` adds `core_agent_clone_opening_events(...)` so hosts can replay a
  source session and materialize clone opening events from the live config,
  tool set, and default tool targets while keeping `SessionStore` domain-neutral.

## Goal

Give the session/store layer three capabilities, independent of any agent tooling:

1. **Clone** a session — create a new session whose configuration and resources
   are copied from an existing one, with a fresh event log.
2. **Fork** a session — create a new session that inherits an existing session's
   event history *by reference* up to a branch point, then continues its own log.
3. **Link** sessions — record directed, typed relationships between sessions.

These are plain store/runtime operations. Nothing here requires an agent, a tool
call, or policy. P83 builds the agent-facing `agent_spawn` / `agent_configure`
surface on top of exactly these primitives, and a human or API client can drive
them directly without P83.

## Why a foundation layer

Clone/fork/link are reusable substrate: API clients, tests, operators, and the
future Fleet control plane all want them. Keeping them free of agent and policy
concerns means they can be built and tested in isolation (`store-pg` +
`SessionStore`), and the engine/workflow need no awareness of forks at all.

The whole foundation is additive to `crates/store-pg/migrations/001_core.sql`:
two nullable columns on `sessions` and one new `session_links` table. No new
durable record types, no registry crate.

## Data Model

### Clone / fork lineage (on `sessions`)

Two nullable columns record where a session's content came from:

```text
source_session_id   content origin; NULL for a fresh root session
source_seq          NULL  -> config-only clone, child log starts at seq 1
                    set   -> history fork; 0 is an empty prefix, otherwise
                             child log continues at source_seq + 1
```

- **Clone**: a new session whose opening config is built from the source
  session's *live* config (model, tools, run defaults), plus copied MCP links and
  environment bindings. Fresh event log at seq 1. `source_seq` is NULL.
- **Fork**: the source's events are inherited **by reference, not copied**.
  `source_seq = N` marks the branch point; the child's own rows start at `N + 1`
  and the child's *effective* log is the source's events `1..N` followed by the
  child's own rows. The child rehydrates `CoreAgentState` by replaying that
  stitched log.

`source_session_id` only records the content origin. It is independent of who
initiated the clone/fork — that provenance, if wanted, is a `session_link` (P83).

### Fork by reference (copy-on-write history)

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

### Session links (new table)

A directed, typed relationship between two sessions:

```text
from_session_id     "from can <relationship> to"
to_session_id
relationship        open string: can_see | can_configure | ... (grows freely)
created_at_ms
metadata
```

Purpose: record which sessions a given session may see, access, or configure. At
the foundation layer this is **plain data** — the store reads and writes it;
nothing enforces it. Enforcement is P83 (capability policy). Links are set
independently of clone/fork lineage and can be created between any two existing
sessions, including sessions that never spawned each other (start session 1,
start session 2, then relate them).

`relationship` is an open string so the vocabulary grows without migration, and
the direction lets access be asymmetric. There is no idempotency column here.

## Cloning Mechanics: What Gets Copied

A session's setup lives in two places, copied very differently.

### In the event log (reconstructed by replay — no row copy)

- `SessionConfig`: model, run defaults, tools.
- MCP links — per `003_mcp.sql`, session-visible links are *materialized into
  tool-set events*, not stored per session in a side table. They ride the log.

These are inherited automatically when the child opens from the source's *live*
config (the fold of `Opened` + every `ConfigChanged`), not the session-start
config — the source may have changed model/tools/links during its lifetime.

For a **fork**, this is automatic: the inherited prefix already contains the
config and link events. For a **clone**, the cloner reads the source's live
config and writes the child's `Opened{config}` from it.

### In per-session side tables (must be re-created for the child)

Both are **one-to-many per session** — a session attaches *many* environments
(PK `(session_id, env_id)`, switched between at call time) and mounts *many*
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
refreshed, never written to the session log, request blobs, or any metadata. A
clone therefore inherits only the *reference*, and mints its *own* token at its own
call time. There is nothing to copy and no token to leak or go stale.

### Inherit then patch

A clone or fork **starts from the source's full setup, then patches** — never the
reverse. The child first inherits the source's complete config and resources, and
any modification is applied as a second step *after* replay, as a new
`ConfigChanged` event (or re-bind/re-mount), never as a rewrite of what was
inherited. This holds identically for clone and fork, and applies to config, MCP
links, mounts, and environment bindings alike. (Who is *allowed* to request which
patch, and the resource-sharing knobs that select share-vs-isolate copying, are
P83 concerns; the primitive simply copies the whole set verbatim and replays.)

### Config-less fork is not a real option

History and config are independent axes, but the log is config-bearing (the first
event is `Opened{config}`), so a fork's inherited prefix inherently carries the
config it asserts. A fork therefore always inherits the source's config from its
prefix; you cannot fork the transcript while starting from a blank model. Config
changes on a fork are allowed only as new events *after* the fork point, never as
a rewrite of the inherited prefix — rewriting it breaks replay coherence and
cache compatibility.

## Fork Point: Never Cut Inside A Run

A fork's branch point (`source_seq`) must be chosen so the inherited prefix is a
replay-safe, provider-valid log. The danger case is forking from a session whose
live head sits mid-turn — an assistant message has emitted a tool call but no
tool result exists yet (this is exactly what happens when P83 forks from inside
the spawning tool call). Forking at the raw head would inherit a dangling tool
call with no result, which most providers reject on the child's first turn.

Because the fork is by reference, the cut point is chosen by **picking
`source_seq`**, not by editing events.

The single invariant: **the cut may land anywhere except inside an open (not yet
terminal) run.** A run's interior is the only place a dangling tool call / open
turn can exist; everywhere else the log is replay-safe.

Default fork point:

- Include *everything* up to the start of the in-flight run — completed runs in
  full **and** the loose non-run events between/before them (`Opened`,
  `ConfigChanged`, context appends, compaction summaries). These are real
  inherited history and are not dropped.
- Exclude only the in-flight run. Concretely, `source_seq` = the seq just before
  the active run's first event (`Run::Accepted`/`Started`). The implemented
  store helper scans CoreAgent run boundary events in the effective log to find
  non-terminal run ranges while keeping the `SessionStore` contract
  domain-neutral.
- If there is no active run at all (forking a quiescent session), the cut is
  simply the head — nothing is open. Excluding the in-flight run is just the
  special case where one exists.

Fallbacks when nothing of this session precedes the open run:

- If only standalone events precede it (e.g. `Opened` + some `ConfigChanged`, no
  completed run yet), the fork still includes them — a short but genuine fork.
- If *nothing* of this session precedes it, cut at the session's start: `0` for a
  root session, or the source's own `source_seq` if it was itself forked
  (pointing at the grandparent's branch point). This is ordinary chain resolution
  with an empty local segment — no special case in the read path, only in the
  recorded `source_seq`.

This generalizes the "fork from the last completed run" intuition: the boundary
is defined by *never cutting inside a run*, not by enumerating run terminals, so
non-run history is preserved and quiescent forks fall out naturally.

The cut-point computation is exposed as a store/runtime helper (given a source
session, return the largest safe `source_seq`) and as an explicit `fork_at_seq`
that callers may pass; an explicit seq that lands inside an open run is rejected.

### Deferred: sanitized forks

Beyond choosing a safe cut point, fork inherits the prefix verbatim by reference.
A sanitized fork (drop incomplete tool calls, raw tool outputs, provider noise;
keep durable user/developer/system facts, final answers, compaction summaries —
per `docs/spec/05-subagents-reference-study.md`) is a later refinement layered on
the same chain resolution, not a change to the storage model.

## Implementation Steps

### G1. Schema

- Add `source_session_id` / `source_seq` columns to `sessions` and the
  `session_links` table in `001_core.sql` (done).
- Add lineage columns to the `store-pg` session record reads/writes (done).

### G2. Side-Table Copy Helper (Mounts And Env Bindings)

This is shared by clone and fork: a child's mounts and environment bindings are
**not** in the event log, so neither config replay (clone) nor by-reference
history inheritance (fork) brings them along. They must be copied explicitly.

- Add a store helper `copy_session_resources(source_session_id, child_session_id)`
  that copies the **full set** of `vfs_mounts` rows and the **full set** of
  `session_environment_bindings` rows from source to child (done).
- Copy **verbatim** (rows point at the same `workspace_id` / `target_id` as the
  source). The share-vs-isolate policy that may instead fork a workspace or
  request a fresh target is **P83**; P82 only does the literal copy.
- Do nothing for universe-scoped catalogs (`mcp_servers`, `auth_*`,
  `environment_providers/targets`, `vfs_workspaces`): the same-universe child
  already resolves them.
- Both `create_cloned_session` and `create_forked_session` call this helper after
  the child row exists (done).
- Live-test that a child with N source mounts and M source bindings ends up with
  exactly those N + M rows, pointing at the same workspace/target ids, and that no
  catalog rows are touched (done in ignored `store-pg` live test).

### G3. Clone And Link Store Methods

- Extend `SessionStore` with `create_cloned_session(source, config_source)` and
  link CRUD (`upsert_link` / `list_links`) (done).
- Implement clone in `store-pg`: open a fresh child session at seq 1 with the
  source session's live config and MCP links (from the log), then call the G2
  side-table copy helper. Do nothing for universe-scoped catalogs (done; callers
  pass opening events materialized with `core_agent_clone_opening_events`).
- Unit-test clone (opens at seq 1 with the source's config and the copied
  side-table rows) and a manual link between two pre-existing sessions (persists,
  reads back, direction preserved) (done via in-memory unit and ignored
  `store-pg` live coverage).

### G4. Fork Creation

- Extend `SessionStore` with `create_forked_session(source, source_seq)` (done).
- Write the child row with `source_session_id` + `source_seq`, seed its head from
  `source_seq` so the first append lands at `source_seq + 1`. No event copying.
  Then call the G2 side-table copy helper (mounts/env bindings are not inherited
  by the by-reference history) (done).
- Add the safe-cut-point helper: given a source session, return the largest
  `source_seq` not inside an open run, applying the empty-prefix fallbacks
  (done).
- Unit-test fork creation (`source_seq` round-trips, first append is
  `source_seq + 1`, side-table rows copied) and the cut-point helper (excludes an
  in-flight run, keeps standalone non-run events, falls back to session start /
  source `source_seq`, rejects an explicit seq inside an open run) (done via
  in-memory unit and ignored `store-pg` live coverage).

### G5. Fork Read Resolution

- Implement chain-aware reads in `store-pg` behind `SessionStore`: `read_after`
  and `head` resolve the `source_session_id` chain recursively, stitching
  `parent[1..source_seq] ++ ... ++ self[source_seq+1..]` into one contiguous seq
  line, clamping each upstream segment to its child's `source_seq` (done).
- Keep this fully behind the store trait: engine/workflow rehydration is unchanged
  and unaware of forks (done).
- Unit-test multi-level forks (fork of a fork), a window spanning a cut point, and
  clamping (parent appends after the fork are invisible to the child) (done).

### G6. Rehydration Check

- Confirm a forked session rehydrates `CoreAgentState` correctly from its stitched
  log (config from the inherited `Opened`, no dangling tool batch), via an
  in-process runner test that forks a session and runs one turn on the child
  (done).

## Deferred

- Sanitized forks (dropping incomplete tool calls / raw outputs / provider noise).
  Verbatim by-reference fork ships here; sanitization layers on the same chain
  resolution later.
- Share-vs-isolate copying of mounts and environment bindings (the primitive
  copies the whole set verbatim; isolation is a P83 spawn knob).
- Any agent-facing surface, access enforcement of `session_links`, and spawn
  idempotency — all **P83**.

## Acceptance Criteria

- The store can clone a session: the child opens at seq 1 with the source's live
  config and a verbatim copy of the source's mounts and environment bindings.
- The store can fork a session: the child's effective log reads the source's
  events up to the branch point (by reference, recursively for a fork of a fork)
  followed by its own, and a parent append after the fork does not appear in the
  child (clamping). The fork also receives a verbatim copy of the source's mounts
  and environment bindings (these are not carried by the inherited history).
- The fork cut-point helper never returns a seq inside an open run, keeps
  standalone non-run events, and falls back to session start / source `source_seq`
  when nothing precedes the open run.
- Two sessions started independently can be put into a `session_link` and the
  link reads back with its direction and relationship preserved.
- The engine and workflow read a forked session through the normal `SessionStore`
  contract with no fork-specific code.
- A forked session rehydrates and runs a turn without a dangling tool call.
