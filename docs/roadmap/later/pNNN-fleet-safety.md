# P93: Fleet Safety — Deferred Budgets And Tree Index

**Status**
- Proposed 2026-07-07, split out of the original draft of
  **[P92](../p92-unified-suspension.md)**.
- **Substantially revised 2026-07-08**, after P92 steps 1–6 landed. The
  original design here — capability tiers, attenuation, topology limits,
  hop counts, tree-local await-cycle rejection — is superseded: P92's final
  mechanism already provides or structurally obsoletes most of it. What
  remains is smaller and sharper: spawn budgets and a tree/active-work
  index. See "What P92/P94 Already Settled" and "Deleted From The Original
  Design" for the accounting.
- **Deferred 2026-07-08**, after **[P94](../p94-engine-native-suspension.md)**
  landed and the base concurrency tools moved out of Fleet. P92/P94 fixed
  the immediate correctness failures: suspension is engine-native, messages
  are log-backed, revocation is ownership-gated, and promise tools are now a
  generic concurrency surface. This document is a backstop plan to resume if
  we see evidence of runaway delegation, reaper scan cost, or tree-level
  spend issues. It is not a current implementation commitment.
- Motivated by the 2026-07-06 production incident described in P92's
  Background: a one-line WhatsApp question produced a depth-3 subagent tree,
  duplicate message floods, and an unrecoverable session — with no
  misbehavior required under today's defaults.
- Builds on **P82 (session graph/links)**, **P83 (Fleet Subagent Control
  Plane)**, **P92 (unified suspension)**, and **P94 (engine-native
  suspension)**.

## Goal

If this is resumed, keep it to store-side backstops. Do not reintroduce
Fleet-specific suspension policy or workflow machinery:

1. **Spawn budgets** make delegation bounded: inherited, decremented limits
   on depth and fan-out, enforced at spawn admission. The one incident
   hazard P92 did not fix is unbounded spawn recursion; this fixes it.
2. **The tree index** makes delegation observable and repairable: tree
   identity on every session, an active-work view the reaper can scan
   without replaying every log, the downward sweep for hard-terminated
   ancestors, and per-tree forensics as one query instead of event
   archaeology across five sessions.

P92/P94 made suspension, cancellation, and mailbox delivery *correct*.
P93 is only for the remaining question: whether we need product-level
bounds and observability for delegation trees.

## What P92/P94 Already Settled

The original P93 draft was written against the pre-P92 surface
(`agent_send { expect: reply }`, `agent_reply`, subscriptions). P92's final
shape closed most of the policy questions the tier system existed to answer:

- **Revocation is ownership-gated.** `cancel`/`await`/`detach` replay only
  the caller's own session state; a promise the caller does not hold is an
  unknown id. No cancel ACL is needed at any capability level (P92 §4).
- **Reach is edge-gated.** The only session links that exist are spawn edges
  (`FLEET_CHILD_RELATIONSHIP`), and `agent_send`/`agent_request` require a
  direct link edge. The reachable set of any session is exactly {parent, own
  children} — today, with no tier machinery, and with no escape hatch.
  Sibling, grandparent, and cross-tree targets are `NotReachable`.
- **Request cycles are unconstructible.** `agent_request` rejects parent
  targets, all edges are parent↔child, and spawn always creates a fresh
  node — so request edges point strictly downward in a forest. The
  mutual-request await deadlock documented as a P92 residual requires two
  linked non-parent sessions, which the current linking model cannot
  produce.
- **Leaves need no fleet tools.** Completion is the response to delegated
  work (P92 §5): a worker returns its answer by finishing its run. The
  old worker tier existed for the reply-capability dimension that
  `agent_reply`'s deletion removed.
- **The mailbox is bounded.** Queue cap with backpressure, fail-fast to
  terminal sessions, duplicate sends within one batch rejected (P92 §5).
- **Zombies die with their creating run.** Run-scoped promises auto-cancel
  on any terminal state and cascade outward; the reaper repairs broken
  promise edges (P92 §4).
- **The mailbox is engine state.** P94 made buffered messages, parked
  awaits, and message consumption/promotions log-backed and projectable.
  A tree index can project those facts from engine state; it does not need
  a Fleet-specific delivery model.
- **Concurrency is not Fleet.** `await`/`cancel`/`detach` are generic
  concurrency tools, and timer promises use the same surface through
  `sleep`. Fleet safety should not own promise-tool policy.

What P92/P94 still do not bound: a session can spawn without bound (the
incident's depth-3 recursion is still legal), a tree's total spend is not
capped, the reaper full-replays every session log every five minutes, and
the tree-root downward sweep does not exist. This document keeps those
backstops designed, not scheduled.

## Design

### 1. Tree identity

Every session carries `tree_id`, `parent_session_id`, and `depth`, recorded
at creation and immutable:

- A client-created session is its own tree root: `tree_id = session_id`,
  `depth = 0`, no parent.
- `agent_spawn` stamps the child with the parent's `tree_id`,
  `parent_session_id`, and `depth + 1`.

These are store-side session-record fields, indexed (§3). They are identity,
not policy: budgets read them, the index queries by them, and the `tree_id`
becomes a Temporal search attribute for operator queries. The spawn link
edge (P82) remains the authoritative graph edge; `tree_id`/`depth` are its
denormalization for cheap enforcement and query.

### 2. Spawn budgets

Keep `tools.fleet: bool` as the coarse tool-surface switch. The budget is
separate enforcement policy for spawn admission, not a replacement for the
toolset's enabled flag. Exact API placement can wait until implementation,
but the shape is:

```jsonc
"tools": { "fleet": true },
"fleet": {
  "spawn_budget": {
    "depth": 1,
    "children": 4,
    "descendants": 8
  }
}
// or "fleet": false — no fleet surface at all
```

- **`depth`** — remaining spawn depth. A spawned child inherits
  `depth - 1`; at `0`, `agent_spawn` and `agent_request` toward new children
  are rejected with a budget error. *A worker is a session spawned with
  depth 0* — the old `worker` tier, without the vocabulary. `spawn: false`
  is depth 0 at the profile root.
- **`children`** — max live (non-closed) children of this one session.
- **`descendants`** — max sessions in the whole tree, enforced at every
  spawn anywhere in it.
- **Inheritance**: v1 should derive child budget from the parent rather
  than letting `agent_spawn` request budget grants. Depth decrements; other
  limits stay bounded by the inherited policy/tree cap. If explicit child
  grants are added later, enforce monotonic grants: a child may receive
  less than the parent has left, never more.

Enforcement lives at spawn admission in the fleet service, reading the tree
index: `depth` is a field comparison; `children` and `descendants` are index
counts. **Enforcement is approximate under concurrency, by choice**: two
racing spawns may overshoot a cap by the width of the race. A safety budget
does not need a transactional counter — the alternative (serializing all
spawns in a tree through a counter row) buys exactness nobody needs at the
cost of a contention point on the hot path.

Defaults are conservative: `depth: 1`, `children: 4`, `descendants: 8`.
Under these defaults the incident's tree is cut at the grandchild spawns:
the bridge session spawns two children, and the middle child's own spawns
are rejected at depth. Orchestrator profiles raise limits explicitly.

`agent_request` needs no budget of its own in v1: it creates runs, not
sessions, and the receiving session's queue cap already bounds it. The
generic bound on run volume is the tree budget (§4).

### 3. The tree / active-work index

One store-side view, three consumers. The index maintains, per session:
`tree_id`, `parent_session_id`, `depth`, lifecycle status, and an
active-work summary: active/queued runs, parked await deadline/mailbox
state, buffered message count, and pending promises. P94 made those facts
log-backed engine state; the index is a projection, not a new source of
truth. In `store-pg` this can be updated with appends; `store-fs` may fall
back to an incremental cursor over logs. The requirement, if this becomes
necessary, is behavioral: **a reaper pass should be O(sessions with pending
work), not O(total session history)**.

Consumers:

1. **Observability** (§5): `agent_list --tree`, per-tree spend rollups,
   alert queries.
2. **The reaper**: skip closed sessions outright, select only sessions whose
   summary shows active work, and replay only those logs to plan repairs.
   Same repair logic as P92 step 4a, different discovery.
3. **The downward sweep** — the piece P92 deferred. Definition of *dead*:
   a session is dead when its log says `Closed`, **or** its log says open
   but its workflow is not running. The second arm is precise in this
   architecture because a live open session always has a running workflow
   (continue-as-new keeps it alive; the workflow completes only after
   close) — open-with-no-workflow is by construction an anomaly, and it is
   exactly the incident's end state. The sweep: for each open spawned
   session whose parent is dead, cancel its active work through the P92 §3
   funnel and close it via the registry-level force-close path. Recursion
   is emergent, matching P92's teardown model: closing one level makes it
   dead for the next pass. The sweep is idempotent under concurrent reapers
   for the same reasons the edge repairs are (first-writer-wins admissions,
   expected-head-protected direct appends).

### 4. Tree run/token budgets — the loop-breaker

The one runaway shape that survives edge-gating, queue caps, and spawn
budgets: message ping-pong. Parent and child `agent_send` each other, each
message waking or enqueueing on the other — every hop is edge-legal and
queue-legal, and the tree burns tokens while looking healthy.

The bound is a per-tree cumulative budget: max total runs and/or max total
tokens, checked at run admission against the index rollup (approximate,
like §2, and for the same reason). A tree over budget rejects new runs with
a `budget_exhausted` error the model can see, freezes rather than wedges —
in-flight work completes, awaits still resolve, close still works — and
fires an alert (§5).

This deliberately replaces the original draft's hop-count/provenance
enforcement: a run/token budget bounds *every* runaway shape — relays,
self-inflicted retry storms, degenerate planning loops — where hop counts
bound only message chains. Provenance metadata on fleet messages may still
ride along later for observability; it is not an enforcement mechanism.

### 5. Observability

- `tree_id` as a Temporal search attribute; one query returns the incident's
  whole tree. The 2026-07-06 forensics required event archaeology across
  five sessions; that becomes `agent_list --tree` / one operator query.
- Per-tree token and run spend, rolled up from the index.
- Alerts: `cancelling > 60s` (the watchdog should make this rare — firing
  means a watchdog regression), mailbox depth near cap, `budget_exhausted`
  events, and orphan count (open sessions with a dead parent should be
  transient once the sweep runs — a non-zero steady state means the
  backstop is broken).

## Deleted From The Original Design

Each of these was in the 2026-07-07 draft; each is deleted for cause, not
deferred by neglect:

- **Capability tiers (`worker | coordinator | full`).** The lattice
  collapsed: revocation is ownership-gated (P92 §4), reach is edge-gated by
  the link graph, the reply-capability dimension died with `agent_reply`,
  leaves need no fleet tools because completion is the response, and "may
  spawn" is just "remaining depth > 0". Every question a tier answered is
  answered by a budget field or by P92 structure. A named tier vocabulary
  would be a second, coarser encoding of the same facts — prompt-legibility
  belongs in profile prompts, not in an enforcement enum.
- **Attenuation ("child defaults one tier down").** Subsumed: budget
  inheritance decrements. Attenuation never bounded depth (the original
  draft admitted this); the decrement does.
- **Tree-local await-cycle rejection.** Nothing to reject: request edges
  point strictly downward, so in-tree await cycles are unconstructible.
  This becomes a structural test (below) plus the P92 residual note, and is
  revisited only if a future feature creates non-spawn link edges — the
  gate belongs on that feature, not here.
- **Hop-count/provenance enforcement.** Replaced by tree run/token budgets
  (§4), which bound strictly more failure shapes.
- **Cross-tree sends for `full` profiles.** There are no cross-tree edges
  to send across. When an explicit session-linking feature lands, it
  carries its own policy; building the tier for it now would be gating a
  feature that does not exist.

## Deferred Implementation Sketch

Do not implement this by default just because the document exists. Resume
only if production evidence shows runaway delegation, reaper scan cost, or
tree spend requires it. If resumed, split it into small independently
shippable changes:

1. **Tree identity + budget config + spawn enforcement.** `tree_id` /
   `parent_session_id` / `depth` on session records, stamped at create and
   spawn. Keep `FleetToolsetConfig { enabled: bool }`; add separate spawn
   budget policy only where admission needs it. Enforce
   depth/children/descendants at spawn admission (counts may be simple
   store queries before the index lands; the limits are small).
2. **Tree/active-work index + reaper adoption + downward sweep.** The
   indexed active-work view; reaper discovery switches from full-log replay
   to index selection (retiring P92's cost caveat); the dead-ancestor sweep
   with the open-with-no-running-workflow definition of dead.
3. **Tree run/token budgets + observability.** Index-backed spend rollups,
   `budget_exhausted` at run admission, `agent_list --tree`, search
   attribute, alerts.

## Tests

- **Budgets**: depth-0 spawn rejected; children and descendant caps enforced;
  `fleet: false` exposes no fleet tools; if explicit child budget grants
  exist, a spawn requesting more than the parent can grant is rejected.
- **Incident replay**: the 2026-07-06 topology against defaults — the
  grandchild spawns are rejected at depth; the tree bottoms out at depth-1
  leaves that hold no spawn capability.
- **Concurrency**: racing spawns may overshoot a cap by at most the race
  width, never unboundedly (property test over interleavings).
- **Structural (replaces cycle rejection)**: request-to-parent rejected;
  all link edges are parent↔child; therefore no sequence of
  spawn/request/send calls constructs an in-tree await cycle.
- **Index**: a reaper pass reads only sessions with active work; closed
  sessions are skipped; results match the full-replay reaper on the same
  store (equivalence test while both exist).
- **Sweep**: descendants of a hard-terminated parent (open log, no running
  workflow) are cancelled and closed recursively across passes; idempotent
  under two concurrent reapers; a live open parent is never treated as dead.
- **Tree budgets**: an over-budget tree rejects new runs with
  `budget_exhausted`, completes in-flight work, and still closes cleanly; a
  send ping-pong loop freezes at the run budget.

## Key Decisions

Deliberate choices, argued in the referenced sections:

- **Budgets, not tiers** — every tier question is a budget field or already
  answered by P92 structure; no second encoding (§2, "Deleted").
- **A worker is depth 0** — leaf delegation is just no remaining spawn
  depth (§2).
- **Approximate enforcement** — safety budgets tolerate race-width
  overshoot; no transactional spawn counter (§2).
- **Dead ≡ closed, or open with no running workflow** — precise because a
  live open session always has a running workflow; this is the incident's
  exact end-state signature (§3).
- **Run/token budgets over hop counts** — bound all runaway shapes, not
  just message relays (§4).
- **One index, three consumers** — observability, reaper discovery, and the
  downward sweep share the active-work view; the reaper's O(total history)
  caveat retires here (§3).
- **Cycle policy waits for the feature that could create cycles** —
  downward-only request edges make in-tree cycles unconstructible today;
  the structural test pins it (§ "Deleted").
