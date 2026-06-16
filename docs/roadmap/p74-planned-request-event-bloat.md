# P74: Planned-Request Event Bloat

**Status**
- Proposed 2026-06-16.
- Split out of `docs/roadmap/p73-session-rehydration-payload-limit.md`. P73 was
  the `ls.bot` Hetzner incident (`Complete result exceeds size limit` at
  bootstrap). P73 fixes the *transport* (compact bootstrap/continue-as-new); P74
  fixes the *durable log growth* that made the log large in the first place.

## Problem

Every `lightspeed.core.turn.planned` event embeds a full `LlmRequest`:

```rust
// crates/engine/src/core/components/turn.rs
Planned {
    turn_id: TurnId,
    run_id: RunId,
    request: LlmRequest,
},

// crates/engine/src/core/components/llm.rs
pub struct LlmRequest {
    pub model: ModelSelection,
    pub request_fingerprint: String,
    pub context: ContextSnapshot,   // <- full active-context entry list
    pub tools: Vec<ToolSpec>,       // <- full tool catalog
    pub tool_choice: Option<ToolChoice>,
    pub output_limit: Option<u32>,
    pub provider_response_id: Option<String>,
    pub compaction: Option<CompactionPolicy>,
    pub params: Option<ProviderParams>,
}
```

`ContextSnapshot.entries` is a `Vec<ContextEntry>` of every active entry; `tools`
is the full catalog. The *payloads* are already content-addressed — a
`ContextEntry` holds `content_ref: BlobRef`, not inline bytes, and a `ToolSpec`
references a `description_ref` rather than inlining the schema. What is repeated
is the **metadata list**: for every turn we re-serialize the id/key/kind/source/
preview/token-estimate of every active entry, plus the tool catalog, into a new
durable event.

In the incident, ~537 events were ~1.1 MB as Postgres JSONB / ~2.5 MB as JSON
text, dominated by these repeated planned-turn snapshots.

## Why This Grows — And Why "We Still Send All Items" Is Not The Bug

It is tempting to think the growth comes from sending every context item to the
provider on every turn. It does not, and that behavior is correct.

Separate two different quantities:

1. **The provider request size, per turn — `O(active context)`.** A stateless
   chat/completions API has no memory; each turn must carry the full active
   context. This is inherent and *bounded*: when the context window fills,
   compaction prunes/summarizes active context (`ContextState.entries` shrinks
   on `StateReplaced` / compaction), so the live entry list oscillates up to the
   window and back down. The materialized request the provider sees never grows
   without limit. **Sending all active items every turn is fine and stays
   bounded by the window.**

2. **The durable event log size, cumulative — `O(Σ active context over turns)`.**
   The bug is that we *persist* a fresh full copy of that active-context list
   into a new `turn.planned` event every turn, and keep it forever. With `N`
   active entries held roughly flat by compaction over `T` turns, the live
   request stays ~`N`, but the log has stored ~`N·T` entry-metadata copies. That
   product is the growth. Compaction caps quantity (1); it does nothing for (2),
   because compaction shrinks *live* context, not the historical events that
   already snapshotted it.

So the resolution to the confusion: we keep sending all active items to the
provider (correct, bounded by the window). What we stop doing is **durably
recording a fresh snapshot of that list in every event** (and in reduced state).
The request stays `O(context)` when materialized; it is just no longer
*persisted* anywhere — it is built transiently at generation time and discarded.
Compaction bounds the materialized request; P74 bounds the log.

Concretely, the dominant term to kill is the repeated `ContextSnapshot.entries`
+ `tools` lists inside `turn.planned`. Everything else in `LlmRequest` is small
and constant-ish per turn.

## History / How We Got Here

- `LlmRequest` was designed provider-neutral and self-contained so the runtime
  could materialize a wire request without re-reading state — convenient, and it
  made `turn.planned` a complete audit record of exactly what was planned.
- Entry *contents* and tool *schemas* were moved behind `BlobRef`s
  (`content_ref`, `description_ref`) to keep large payloads out of events. This
  solved payload bloat but left the *list of refs+metadata* inline.
- For short sessions this is invisible. The cost only shows up in long-lived
  bridge sessions (`ls.bot`), where turn count is high and the same active set
  is re-snapshotted each turn — exactly the sessions P73's incident came from.

The original instinct (a self-contained, replayable, auditable planned request)
turns out to be the thing to drop. The request does not need to be *persisted* at
all: the active set is already reconstructable from the durable context events
the reducer replays anyway, so the planned request can be rebuilt on demand and
otherwise built transiently at generation time.

## Sizing — Why The List Must Leave The Durable Log

A `ContextEntry` serializes to roughly 300–450 bytes of JSON (dominated by its
`sha256:` `content_ref`, plus `kind`/`source`/`preview`/`token_estimate`); a
`ToolSpec` to roughly 200–350 bytes. So one full active-set snapshot is on the
order of:

| active set | one full list |
|---|---|
| 20 entries / 10 tools | ~8–12 KB |
| 50 entries / 20 tools | ~19–29 KB |
| 150 entries / 40 tools | ~52–80 KB |

Re-recording that **inline in every `turn.planned`** gives durable-log growth of
`O(N·T)`:

| active set | T=500 turns | T=2000 turns |
|---|---|---|
| 20 / 10 | ~0.8–6 MB | ~15–24 MB |
| 50 / 20 | ~9–14 MB | ~36–56 MB |
| 150 / 40 | ~25–39 MB | ~100–155 MB |

The incident hit ~1.1 MB at 537 events; these projections show why the curve is
the real problem, not that one session. The dominant term is the repeated list.

### Where the list can live, and the cost of each

There are three logs/stores in play, and they age very differently:

| store | lifetime | scanned at bootstrap? |
|---|---|---|
| durable `session_events` | forever (append-only audit) | **yes — replayed** |
| reduced `CoreAgentState` | carried across continue-as-new | yes (it *is* bootstrap output) |
| Temporal workflow history | **reset by continue-as-new** | no |
| CAS | forever, content-addressed | only on-demand fetch |

The key asymmetry: **Temporal history is the only place that gets garbage-
collected for free**, via continue-as-new. Data that lives only as transient
activity input/output there is bounded by the continue-as-new interval, not by
total session length.

## Decision

Get the list out of the durable log *and* out of reduced state entirely. Do not
persist the materialized request anywhere — not inline, not as a CAS blob.

- **`turn.planned` records only**: `turn_id`, `run_id`, `request_fingerprint`,
  `context_revision`, `toolset_revision`. No entry list, no tool list, no CAS
  ref. Per-turn durable cost is a fixed handful of bytes → `O(T)` total, with
  **zero CAS churn**.
- **The reducer stops storing the request in `CoreAgentState`.** Today
  `apply_event` stores `active_turn.request = Some(request.clone())`
  (`turn.rs:294`) after validating it against the active context the reducer
  already maintains. Replace that with: validate `context_revision ==
  state.context.revision` (a cheap integer check that subsumes the current
  list-equality check at `validate_request_matches_active_context`), and store
  only `request_fingerprint` + revisions in `TurnState`. The reducer already
  knows the active set from the durable context events
  (`EntriesApplied`/`EntriesRemoved`/`StateReplaced`) it replays — the request's
  copy was always redundant *to the reducer*.
- **The request is built transiently at generation time.** The workflow already
  holds the reduced active set in `CoreAgentState.context.entries` (metadata +
  `content_ref`, the bounded thing P73 carries). It passes that list as input to
  the generation activity, which materializes the wire `LlmRequest` in memory,
  calls the provider, and returns compact facts. The full list exists only as
  activity input — i.e. only in Temporal history, which continue-as-new resets.
  It never lands in the durable log, in reduced state, or in CAS.
- **Audit/debug rebuilds on demand.** The full provider-neutral `LlmRequest` for
  any past turn is a pure function of the durable context events up to its
  `context_revision` plus the small recorded fields. No stored artifact is
  needed to reconstruct it.

This is the original P73 "materialization activity" idea, kept to its good half:
the request is materialized in an activity rather than persisted. It avoids the
bad half — there is **no cursor, no manifest blob, no hash-verification, and no
second delta-reducer inside the activity** — because the workflow hands over the
already-reduced active set instead of asking the activity to re-derive it from
deltas.

### Why not a CAS blob / manifest per turn

An earlier draft proposed writing the list to a CAS blob ("manifest") per turn
and referencing it from the event. Two reasons that was dropped:

1. **It only relocates bytes.** Writing the list to CAS instead of inline moves
   it off the *replay scan* path (a genuine win) but writes the same `O(N)` bytes
   per change. The sizing above shows that the win is "off the replayed log",
   not "fewer bytes" — and once the goal is "off the replayed log", a transient
   activity input achieves it with **no persistent write at all**.
2. **CAS dedup only helps when context is stable across turns.** But each turn
   appends new messages/tool results to the active set, so the list typically
   changes nearly every turn — pushing dedup toward zero benefit while still
   paying per-turn blob writes and adding a CAS-GC edge. Transient
   materialization sidesteps this entirely.

A `request_ref` in CAS remains available as a *later* opt-in if on-demand rebuild
proves operationally awkward (e.g. a debugger that can't run the rebuild), but it
is explicitly not the default, because it re-adds per-turn churn.

## Proposed Fix

### G1: Compact `turn.planned`

Refactor `TurnEvent::Planned` to record `request_fingerprint`,
`context_revision`, and `toolset_revision` instead of `request: LlmRequest`.
This is a versioned engine event change: gate on a planner/event version and keep
a decode path for existing `turn.planned` events that still inline a full
`LlmRequest` (old sessions must still replay). Do not require backfill.

### G2: Reducer stops storing the request

Change `apply_event` for `Event::Planned` to validate `context_revision`/
`toolset_revision` against current reduced state and store only
`request_fingerprint` + revisions in `TurnState`, not the `LlmRequest`. Drop the
inline list-equality check in favor of the revision check. The reducer remains
deterministic and reads only the durable context/tool state it already maintains.

### G3: Transient generation materialization

The generation path takes the workflow-held active set
(`CoreAgentState.context.entries`) and tool catalog, materializes the wire
`LlmRequest` in memory inside the generation activity, calls the provider, and
returns compact `LlmGenerationFacts`. Nothing materialized here is persisted. The
list crosses only the activity boundary (Temporal history, reset by
continue-as-new).

### G4: On-demand request reconstruction

Provide a deterministic function that rebuilds the full `LlmRequest` for a past
turn from the durable context/tool events up to its recorded `context_revision`/
`toolset_revision` plus the small recorded fields, for audit/debug/replay. This
is a read path, not a write path.

### G5: Metrics and regression guards

- Per-event serialized size metric and a per-session cumulative-size metric.
- A test that runs many turns over a stable active context and asserts the
  durable log grows `O(turns)` in small fixed-size events — not `O(turns × N)` —
  and that **no per-turn CAS blob** is written for the planned request.
- An assertion that `turn.planned` carries no inline `ContextSnapshot`/tool list
  on the new event version, and that `TurnState` no longer holds a full request.
- A test that on-demand reconstruction rebuilds a request equal to what was sent,
  from durable events alone.

## Relationship to P73

- P73 fixes bootstrap/continue-as-new transport and is the urgent incident fix.
  It works regardless of log size.
- P74 reduces how fast the durable log grows. With both, a long bridge session
  both bootstraps compactly (P73) and accumulates slowly (P74).
- P73 deliberately does **not** depend on P74: shipping P73 alone resolves the
  outage; P74 can land afterward without re-touching the bootstrap path.

## Acceptance Criteria

- `turn.planned` on the current event version records `request_fingerprint`,
  `context_revision`, and `toolset_revision` only — no inline `LlmRequest` and no
  request CAS ref; older inlined events still decode and replay.
- `TurnState` no longer holds a full `LlmRequest`; the reducer validates by
  revision and stores only fingerprint + revisions.
- The `LlmRequest` for a past turn can be rebuilt deterministically from durable
  context/tool events plus the small recorded fields, for audit/debug/replay.
- A regression test with `T` turns over a stable `N`-entry active context shows
  durable-log growth linear in `T` (small fixed-size events), and **no per-turn
  CAS blob** written for the planned request — not `~N·T`.
- The engine reducer remains deterministic; the full request is materialized only
  transiently in the generation activity and is never persisted.
