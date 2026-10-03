# P147 — Session Checkpoints and Bounded Reads

**Status**

- Review-fix pass 2026-09-01, after implementation review: run detail became
  one complete projection (stateful run projection must never be paged),
  channel delivery reads full reply bodies from CAS and tolerates unknown
  runs, the executing run is a dedicated `activeRun` session fact so steering
  never depends on a page window, `blobs/read` is exempt from the response
  byte budget, cloned sessions checkpoint correctly, approval summaries carry
  the real request time, CLI steering/interrupt track runs from the event
  tail, and the Configurator emits one text representation.
- Context-sourced runs removed 2026-09-01: `source=context` run starts had no
  first-party caller — bot deliveries start input runs and ambient listening
  is `context/append` — while the machinery (trigger validation, resolved
  trigger refs, dedicated view variants) taxed every run-source change and
  produced the pre-acceptance projection hole. Every run now starts from
  explicit input; referencing an existing document is an `InputItem::TextRef`
  at the same blob, and run-to-entry provenance can return later as an
  optional annotation on input runs, not a distinct source kind.
- Implemented 2026-09-01. Session bootstrap and gateway state reads now use
  validated CAS-backed reducer checkpoints plus an authoritative fenced tail;
  PostgreSQL checkpoint pointers advance monotonically at schema revision 11.
  Session reads expose bounded run summaries, run list/detail have keyset and
  sequence pagination, the four session mutation responses are compact, and
  generated Rust/TypeScript consumers use the new contract.
- Verification includes reducer cut-point equivalence, corrupt-checkpoint
  fallback, stale-pointer rejection, summary pagination, workspace Rust tests,
  TypeScript type checks/tests/builds, generated-contract checks, and release
  metadata verification. Focused PostgreSQL, hosted Temporal, native MCP, and
  paid OpenAI live tests pass against the local development stack.
- Proposed 2026-09-01; revised 2026-09-01 after a design review against the
  current code. The first draft added a transactional `session_runs` read
  model plus indexed event joins alongside checkpoints. The revision derives
  run summaries from checkpointed reducer state instead and defers any SQL run
  index until a measured session needs one. It also corrects the draft's
  description of the live-update path and adds two read-path defects
  (quadratic projection, sequential blob resolution) that dominate observed
  latency today.
- Motivated by a Configurator MCP self-inspection failure where
  `session/read` materialized about 1.28 MiB of session data and the MCP
  wrapper duplicated it into a 2.72 MiB response, and by multi-second
  `session/read` latency on long-lived test sessions.

## Problem

`session/read` currently has one parameter, `sessionId`, and returns one
`SessionView` containing every run with fully inlined, untruncated
blob-backed text. The gateway path is: load every effective session event,
reduce the complete log into `CoreAgentState`, project every run, resolve
every projected blob, serialize everything.

P73 made workflow bootstrap safe by reducing inside the activity and
returning only compact state through Temporal. It did not make reduction
incremental: bootstrap and gateway reads still replay from sequence 1.

The read path has four distinct cost layers:

1. **Event read and replay** are linear in total log length, on every read.
2. **Projection is quadratic**, not linear: `project_run_with_api_status`
   rebuilds a projection over the entire entry slice for each run, and every
   per-run helper (accepted source, usage, lifecycle timestamps, context
   entries, pending approvals, tool batches) rescans the whole slice filtered
   by `run_id`. Cost is O(runs × total events).
3. **Blob resolution is strictly sequential**: one awaited `read_text` per
   blob-backed entry inside plain loops, with no batching. The CAS blob cache
   makes repeats cheap but a cold read serializes one round trip per entry.
4. **Serialization inlines untruncated blob bodies** into
   `ContextEntryView.text`, while the adjacent approval projection already
   truncates at 4 KiB.

Several amplifiers multiply that cost:

- Four mutation responses (session start, config put, context compact,
  session close) embed the same complete `SessionView`.
- The web client already follows `session/events/read` by long-poll for the
  transcript — that path is bounded and correct — but it refetches the full
  `SessionView` on every run-activity revision bump to reconcile run status
  and pending approvals. During an active run this triggers repeated full
  replays at a rate proportional to run activity, which is worse than a
  fixed-interval poll. The consumer never reads `run.entries`; a summary view
  would satisfy it.
- The Platform backend instructions editor fetches an entire `SessionView`
  only to read `activeContext.entries`.
- The Configurator MCP wrapper emits every successful result twice: once
  JSON-stringified into `content`, once as `structuredContent`.
- MCP response-limit failures always say "discovery" even when a
  `tools/call` response exceeded the budget, because both paths share one
  budget with hardcoded diagnostics.

Two adjacent facts matter for scoping:

- Live reducer state grows with lifetime run count: `runs.completed` retains
  one metadata-only `RunRecord` per terminal run (turns, tool batches, and
  context entry ids are dropped at terminal), and the workflow keeps a
  parallel `run_submissions` index. Growth is a few hundred bytes per run,
  not transcript-sized. The comment in the workflow config claiming reduced
  state is bounded by active context alone is wrong and must be fixed.
- `safe_fork_seq` materializes the complete effective log to find run
  boundaries, an unlisted full-history read path.

## Decision

Keep the append-only session event log as the sole durable authority. Add
**one** derived layer, not two:

1. A **reducer checkpoint** per session — the raw serialized reduction stored
   as a CAS blob, referenced by a single advance-only pointer row — so
   current-state reads and workflow bootstrap replay only a bounded tail.
2. **Run summaries come from reducer state itself.** `RunRecord` is extended
   with sequence bounds, timestamps, usage totals, and the input blob
   reference — scalars and references only, never resolved content. Summary
   pages are served from loaded state; run detail is a primary-key range scan
   over the run's sequence interval. No `session_runs` table, no generated
   join columns, no transactional projection maintenance, no backfill.

The checkpoint is disposable: if missing, corrupt, stale, or from an
unsupported format, Lightspeed reconstructs state from the event log.
Deleting the pointer row never changes session behavior.

Lightspeed is greenfield: response shapes change outright and all consumers
(web, CLI, Configurator, generated clients) update in the same change. There
is no dual-maintain, shadow-comparison, or compatibility phase.

A durable SQL run index and eviction of terminal records from live state
remain sketched under **Deferred** and are built only when a measured session
approaches the checkpoint budget.

## Target Read Shape

```text
current session state    O(checkpoint bytes + events after checkpoint)
latest N run summaries   O(N) from loaded state, + N bounded preview reads
one historical run       O(events in that run's sequence interval)
event tail               O(requested event page)
full audit replay        O(total history), explicit only
```

Honest bound: state, and therefore the checkpoint, stays linear in lifetime
run count at metadata scale (roughly 200–300 bytes per run). Against the
1.5 MB bootstrap payload budget that is several thousand runs of headroom
per session. The deferred run index removes that term if real sessions
approach it; nothing in this design forecloses it.

`session/events/read` remains the authoritative chronological stream and the
sole per-event live path. Clients must not repeatedly obtain transcripts
through `session/read`.

## Run Records Carry Their Bounds

Engine change, applied by the ordinary reducer so old logs replay into the
new shape without any migration of events:

- `RunRecord` gains `first_seq` (the run-accepted event position),
  `terminal_seq`, accepted/started/completed timestamps, aggregated usage,
  and `source_ref` — the `BlobRef` of the run's input — alongside the
  existing `output_ref` and failure facts. Every value comes from events the
  reducer already applies.
- Approval records retain `requested_at_ms` from the request event, so
  summary and detail projections report the same request time without an
  event rescan.
- **No resolved content ever enters state.** Previews are resolved at read
  time from `source_ref` and truncated at serialization, following the
  existing bounded-truncation precedent. A summary page therefore costs at
  most page-size blob reads, cache-hot after the first request.
- Exactly one run is active at a time, so a run's events occupy a
  near-contiguous sequence interval. The interval may contain interleaved
  accepted/steering events from queued runs; the detail projector filters by
  the stored `run_id` join after decoding the interval.
- Forks come for free: a fork replays its inherited prefix, so inherited
  `RunRecord`s are simply present in child state, and the child checkpoint
  covers them without copying events. Safe-fork-point computation must move
  from materializing the full effective log to reading active/queued run
  bounds out of reduced state, closing that full-history path.

## Reducer Checkpoints

### Storage

The checkpoint blob is the raw serialized reduction — `CoreAgentState` plus
the workflow submission index — content-addressed in CAS. No envelope and no
nesting inside a database JSON column; all metadata lives on the pointer row:

```sql
CREATE TABLE session_checkpoints (
    universe_id uuid NOT NULL,
    session_id text NOT NULL,
    through_seq bigint NOT NULL,
    format_version integer NOT NULL,
    state_digest text NOT NULL,
    lineage_source_session_id text,
    lineage_source_seq bigint,
    byte_len bigint NOT NULL,
    created_at_ms bigint NOT NULL,
    PRIMARY KEY (universe_id, session_id),
    FOREIGN KEY (universe_id, session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE CASCADE,
    FOREIGN KEY (universe_id, state_digest)
        REFERENCES cas_blobs (universe_id, digest) ON DELETE RESTRICT
);
```

- **One row per session.** An advance-only upsert
  (`WHERE through_seq < excluded.through_seq`) means a slower writer can
  never regress the pointer. There is no checkpoint history and no
  multi-generation retention: the fallback for a bad checkpoint is full
  replay, so a second generation buys nothing.
- Replacing the pointer un-roots the previous digest; ordinary CAS
  reachability collection reclaims the old blob.
- The serialized checkpoint must stay under the bootstrap payload budget
  (1.5 MB). That also keeps it under the blob cache's per-entry cap, so
  checkpoint reads are served from the existing immutable, digest-keyed
  cache for free.
- The lineage columns bind the checkpoint to the session's immutable
  effective-history origin (`source_session_id`, `source_seq`). Any mismatch
  with the current session record rejects the checkpoint. A clone's shape —
  source session recorded with no source sequence — is valid lineage; only a
  sequence without a session is malformed.

### Read and validation

1. Load the pointer row; unsupported `format_version` is a cache miss.
2. Fetch and digest-verify the blob; deserialize.
3. Read and decode events after `through_seq` through the current head.
4. Apply the tail with the same reducer used for full replay.
5. Validate `reduced_to` against the head; return the state.

No alternate reducer exists. `apply_event` already enforces
`entry.seq == reduced_to.seq + 1`, so a tail applied onto a checkpoint is
contiguity-checked by construction. Corruption, invariant violations,
sequence disagreement, or lineage mismatch emit checkpoint diagnostics and
fall back to full replay; an error is fatal only if authoritative replay
also fails. Stores without checkpoint support (the filesystem store) simply
always report a miss and replay.

### Accumulator

The per-event fold is already checkpoint-shaped; the work is deduplication,
not invention:

- The rehydration accumulator and the workflow drive's
  apply-entries-onto-live-state helper are near-duplicates of the same fold.
  Unify them, and give the entry-list reducer a seed-from-state constructor.
  Full replay from empty state remains the reference path.
- Bootstrap freshness must stop being inferred from
  `replayed_event_count == 0` — under checkpoints that count becomes
  tail-relative. The bootstrap result carries an explicit fresh-session
  fact derived from the head position.

### Writers and cadence

Checkpoints are written by storage-side runtime code, post-commit, never by
the deterministic workflow and never inside the append transaction. A
checkpoint failure never rolls back or fails a committed append; it emits
bounded diagnostics and the next bootstrap or append retries.

- `create_or_load_session` loads a checkpoint before replay and writes one
  after a full or materially large tail replay.
- The append path may refresh after commit. The existing stored-entry
  run-boundary helper detects terminal run events with a string compare and
  a joins lookup — no decode, no blob reads — so the trigger is near-free.
  Initial thresholds: ten terminal runs since the prior checkpoint, one
  terminal run spanning more than 100 events, 512 tail events, 2 MiB of
  encoded tail, or a 2 MiB append batch, whichever fires first. Operational
  constants, not API fields. Both
  store-pg append implementations (direct append and the fork/clone
  transaction) share the trigger.

No per-session listener, workflow, or resident process is introduced. An
optional maintenance command may checkpoint idle sessions before a
deployment; it is not required for correctness.

## Run Summary and Detail Reads

- **Summaries** are projected from loaded state: page newest-first by
  `run_id` across `completed`, `queued`, and `active`. `RunSummaryView`
  carries identity, status, timestamps, usage, pending-approval facts, and a
  truncated source preview resolved from `source_ref` at serialization time.
  It never inlines messages or tool results.
- **Detail** (`session/runs/read`) looks up the run's bounds in state and
  range-scans the existing primary key over the effective-history window
  (`first_seq` through `terminal_seq`), decodes that interval, filters by
  the `run_id` join, and projects those events, resolving only their blobs.
  Tests must prove parity with filtering the former full-log projection.
- **Run detail is one complete projection, never a page.** Run projection is
  stateful across the run's events (tool batches pair start/complete,
  approvals pair request/decision), so a partial interval silently drops
  cross-event state. The interval is always read to completion through
  bounded internal pages; a run past the detail event ceiling is rejected
  with a typed error and stays readable through `session/events/read`.
- Inline entry text is bounded (`text` carries at most the inline budget,
  with `textTruncated` set when it is a prefix); complete bodies stay
  blob-addressed via `contentRef`. Consumers that transmit content onward —
  channel delivery in particular — must re-read truncated bodies from CAS,
  never forward the bounded prefix.

## API Shape

```text
session/read         current state summary + newest run-summary page
session/runs/list    older/newer run-summary pages by run-id keyset cursor
session/runs/read    one run's complete detailed projection
session/events/read  chronological authoritative event pages (existing)
```

- `session/read` accepts an optional run limit; the server applies a
  conservative default and hard maximum. The response reports explicitly
  whether older runs exist; clients must not infer absence from omission.
- `SessionView.activeRun` carries the executing run's summary as a dedicated
  fact. The paged `runs` window can omit the active run behind newer queued
  runs, so control surfaces (bot steering, run-status gating, the CLI model
  lock) must read it from `activeRun`, never by scanning the page.
- The run cursor is the public string `RunId` used as an exclusive upper
  bound. Keyset semantics mean concurrent new runs never shift older pages,
  avoid JavaScript integer precision hazards, and never re-send session state.
- **Mutation responses stop embedding `SessionView`.** Session start, config
  put, compact, and close return the head position, lifecycle facts, and the
  affected run summary where relevant.
- The instructions editor consumes the active-context portion of the bounded
  `session/read` state instead of a full view.

Two projection fixes land independent of API shape, because they likely
dominate current latency: build the per-run entry index once per request
instead of rescanning the slice per run, and batch or concurrently resolve
blob reads behind the blob-store boundary.

## Live Updates

The event stream is the sole per-event path. The web transcript already
follows `session/events/read` with bounded long-poll and keeps doing so
unchanged. The full-view refetch on every run-activity bump is replaced by
the bounded `session/read`, used only to reconcile run status, queued/active
transitions, and pending approvals — each such reconciliation now costs
O(checkpoint tail + summary page) instead of a full replay.

## Consistency Model

The API is strongly consistent with the committed session head:

- State is loaded once per request — checkpoint plus authoritative tail
  through the head read at the fence. Run summaries are projected from that
  same state object, so state and summaries cannot diverge by construction.
- Run detail events are read at or below the same fence; a concurrent
  append belongs to the next read.
- A checkpoint may lag arbitrarily without making any read stale; no API
  ever serves a checkpoint without replaying its tail.

## Boundedness

Enforce all of:

- default and maximum run-summary page counts, and a run-detail event
  ceiling with a typed rejection instead of a silently partial view;
- existing event-page limits (unchanged);
- checkpoint size under the bootstrap payload budget;
- checkpoint tail thresholds by event count and encoded bytes;
- serialized byte budgets on public API and MCP responses, returning a typed
  limit error or continuation instead of an unbounded document. `blobs/read`
  is exempt: the budget guards accidentally unbounded documents, and a blob
  body has no smaller page to retry with;
- previews and references rather than duplicated large content;
- exactly one MCP representation of a successful management result;
- separately named, independently configurable discovery and `tools/call`
  response-limit diagnostics. Raising the discovery limit is not a fix for
  an unbounded management API.

## Deferred: Durable Run Index and State Eviction

Build none of this now. Triggers that would activate it: a real session's
run count approaching the checkpoint budget (several thousand runs), a need
to query run events without loading session state, or cross-session run
analytics.

The sketch, kept for that day: a `session_runs` summary table maintained
beside the existing shared lifecycle projection helper; a stored generated
`session_events.run_id` column with a partial index (note the `joins` values
are decimal strings, so the `::bigint` cast applies); eviction of old
terminal records from `CoreAgentState` with gateway-side idempotency
resolution so deterministic admission never gains a database dependency.
Adding the generated column later is a cheap, non-breaking migration over
`entry_json` that already exists.

## Implementation Slices

### Slice 1 — No-schema read-path fixes

- Remove the Configurator's duplicated result encoding; emit one structured
  representation. Give discovery and tool-call limit failures accurate
  operation names.
- Replace the per-run full-slice rescans with one per-request run-entry
  index, eliminating the quadratic projection.
- Batch or concurrently resolve blob reads during projection.
- Measure `session/read` latency on a long test session before and after.

### Slice 2 — Run bounds in state and the bounded API

- Extend `RunRecord` with bounds, timestamps, usage, and `source_ref`.
- Reshape `session/read`; add `session/runs/list` and `session/runs/read`;
  slim the four mutation responses; move the web client's reconciliation and
  the instructions editor off full views.
- Regenerate contracts and update web, CLI, and Configurator in the same
  change.

### Slice 3 — Reducer checkpoints

- Unify the accumulator; add the pointer table and CAS blob write/read;
  wire checkpoint-plus-tail into workflow bootstrap and gateway state
  loads with automatic fallback.
- Write checkpoints at bootstrap and bounded append thresholds.
- Make bootstrap freshness explicit; fix the stale state-boundedness
  comment; derive the safe fork point from reduced state.
- Add hit/miss/fallback, lag, tail-size, and duration diagnostics.

## Migration and Rollout

One schema migration adds the checkpoint pointer table (bump the required
schema revision and extend the known-tables list). No backfill, no
dual-write, no shadow phase: the `RunRecord` extension is replay-derived, so
existing logs produce the new state shape on their next reduction, and API
consumers update with the shape change. No migration touches session events.
Rollback is disabling checkpoint reads; the authoritative log and full
replay remain.

## Tests

- Property/generative: for every cut point in a generated event sequence,
  `reduce(full_log) == reduce(checkpoint(prefix) + suffix)`.
- Corruption/version: bad digest, malformed state, unsupported format,
  sequence disagreement, and lineage mismatch each fall back to full replay.
- Concurrency: a stale writer cannot regress the pointer; a read fenced at
  head H never mixes H+1 events or runs into its response.
- Pagination: newest page, middle, final, invalid cursor, clamped limit,
  concurrent append, active and queued runs; an oversized run's detail is a
  typed rejection, not a partial view.
- Parity: interval-scan run detail equals the former full-log projection;
  state-derived summaries equal full-projection summaries.
- Fork/clone: inherited summaries respect the fork point, never expose a
  partial run, and a child checkpoint reconstructs identical effective
  state.
- Rebuild: deleting the pointer row and replaying produces identical state
  and API views.
- Scale: a session with tens of thousands of events serves current state
  from one checkpoint plus a bounded tail and returns N summaries without
  reading or projecting the rest.
- Temporal: bootstrap output for a large historical log stays under the
  payload budget.
- MCP: a large `session/read` through the Configurator stays within the
  tool-call policy with a single representation; an oversized call reports
  `tool call`, not `discovery`.

## Observability

Record bounded structured metrics, never checkpoint bodies or payloads:

- checkpoint hit/miss/fallback reason, format version, sequence lag, tail
  event and byte counts, load/build/write duration, blob size;
- summary page size, preview blob reads, detail interval event counts,
  serialized response bytes;
- full-replay count and reason, warning when a fallback replay exceeds an
  event/byte threshold.

## Non-Goals

- Replacing the append-only session log or Temporal orchestration.
- Persisting resolved message, tool, or media content in checkpoints or run
  records.
- Using database projections as reducer authority.
- Recording checkpoint blobs in Temporal workflow history.
- One listener, workflow, or long-lived process per session.
- Full-text search over session content.
- Deleting or compacting audit events; retention is a separate policy after
  checkpointed recovery is proven.
- The durable run index and state eviction are deferred (see above), not
  rejected.

## Acceptance Criteria

- `session/read` returns a bounded recent run page with explicit
  continuation; clients do not infer absence from omission, and mutation
  responses no longer embed full session views.
- Reading the latest N run summaries loads no unselected run's events and
  resolves at most N bounded preview blobs.
- Current-state reads and workflow bootstrap start from a validated
  checkpoint and replay a bounded tail in the normal case, falling back to
  full replay automatically.
- Checkpoints are rebuildable from the log and never change deterministic
  outcomes; deleting the pointer row is behavior-neutral.
- Forked sessions preserve effective-history semantics without copying
  event payloads; the safe fork point no longer requires a full-history
  read.
- One enormous run cannot create an unbounded session or MCP response.
- Discovery and tool-call response-limit errors identify the operation that
  exceeded its policy, and successful MCP results have one representation.
- A long-lived-session scale test demonstrates read work proportional to
  the checkpoint tail plus the requested page, with the remaining
  run-count-linear term bounded at metadata scale and monitored against the
  checkpoint budget.
- Web run-activity reconciliation costs O(checkpoint tail + summary page),
  with `session/events/read` remaining the only per-event path.
