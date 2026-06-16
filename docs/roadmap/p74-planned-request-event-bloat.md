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
recording a fresh snapshot of that list in every event**. The request stays
`O(context)` when materialized; only its *persistence* changes from
"inline snapshot per turn" to "a single CAS ref per turn". Compaction bounds the
materialized request; P74 bounds the log.

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
is still right. The change is *where* that self-contained artifact lives: in CAS
addressed by one ref, not inline in every event.

## Options

### Option A — Compact event + request in CAS (recommended)

Stop embedding `LlmRequest` in `turn.planned`. Write the canonical
provider-neutral `LlmRequest` to CAS and record a compact event:

```json
{
  "turn_id": 48,
  "run_id": 19,
  "request_ref": "sha256:...",
  "request_fingerprint": "...",
  "context_revision": 123,
  "toolset_revision": 9
}
```

- The reducer does not invent `request_ref`. The engine emits a logical
  "request planned" fact; the runtime writes the materialized request to CAS and
  the workflow records the returned ref (mirroring how other bulky artifacts are
  produced by activities and recorded as refs in P73).
- Per-turn event drops to a handful of small fields. Log growth becomes
  `O(T)` tiny events plus `O(distinct requests)` CAS blobs, instead of
  `O(N·T)` inline metadata.
- CAS dedup helps further: when the active set and tool catalog are unchanged
  between turns, the materialized request differs only by small fields, and the
  large manifests it references can themselves be CAS-deduped.
- Replay rebuilds logical state from compact events; it does not need the full
  request inline. Debugging/audit fetches the request from CAS by ref.

This is the smallest change that kills the dominant growth term.

### Option B — Option A plus a manifest layer

On top of A, store the context entry list and tool catalog as their own CAS
*manifests* (`context_manifest_ref`, `toolset_manifest_ref` + hashes), and have
the request reference the manifests rather than re-listing entries. Turns that
share an unchanged active set share one manifest blob by hash.

- Pro: strongest dedup. Across a stretch of turns with a stable active set and
  catalog, the metadata list is stored once, not once per distinct request.
- Con: more moving parts (two manifest types, hashes, revision tracking).
- This is a refinement of A, not a different direction. Ship A first; add
  manifests only if metrics show per-request blobs still dominate.

### Option C — Generation materialization cursor (the original P73 sketch)

The version originally proposed in P73: a `GenerationMaterializationCursor`
carried in workflow state, plus a `prepare_generation` activity that, given a
base cursor and a target session position, reads persisted context/tool
*deltas*, applies only materialization-relevant patches
(`entries_applied`/`entries_removed`/`state_replaced`/`tools_patched`/…),
reconstructs the target manifests, verifies their hashes against the workflow's
expected hashes, builds the request, and writes it to CAS.

- Pro: never materializes the full list in the hot path; rebuilds incrementally
  from the previous cursor; strongest theoretical efficiency.
- Con: the delta-apply step is a **second, partial reducer living inside an
  activity**. It must enumerate exactly the materialization-relevant context/
  tool event variants and stay consistent with the real reducer forever. Add a
  new context event variant and forget to teach the materializer, and you
  silently build a wrong request (caught only by the hash check, which then
  fails the turn). For a determinism-critical engine this is a sharp,
  long-lived maintenance edge.
- Con: hash-verification + cursor threading is significant surface area for a
  benefit that A+B largely already deliver via CAS dedup.

### Recommendation

Do **Option A** now. It removes the dominant `O(N·T)` term, requires no second
reducer, and composes with P73 (workflow owns logical state; runtime/CAS own the
bulky request). Treat **Option B** as a follow-up if per-request CAS blobs become
the next bottleneck. Treat **Option C** as explicitly deferred: adopt it only if
profiling shows that re-materializing the full request per turn (even with CAS
dedup) is too expensive, and only with a test harness that proves the in-activity
delta-reducer stays equivalent to the engine reducer.

## Proposed Fix (Option A)

### G1: Compact `turn.planned`

Refactor `TurnEvent::Planned` to record `request_ref`, `request_fingerprint`,
`context_revision`, and `toolset_revision` instead of `request: LlmRequest`.
This is a versioned engine event change: gate on a planner/event version and
keep a decode path for existing `turn.planned` events that still inline a full
`LlmRequest` (old sessions must still replay). Do not require backfill.

### G2: Materialize-and-store boundary

The runtime writes the canonical `LlmRequest` to CAS and supplies the resulting
`request_ref` back to the workflow, which records it in the planned event. Keep
the engine deterministic: it emits the logical plan; the side-effecting write is
a runtime/activity responsibility (consistent with P73's split). The generation
step consumes `request_ref`, loads the request from CAS, calls the provider, and
returns compact facts.

### G3: Metrics and regression guards

- Per-event serialized size metric and a per-session cumulative-size metric.
- A test that runs many turns over a stable active context and asserts the
  durable log grows `O(turns)` in small events, not `O(turns × entries)`.
- An assertion that `turn.planned` no longer carries an inline `ContextSnapshot`
  on the new event version.

## Relationship to P73

- P73 fixes bootstrap/continue-as-new transport and is the urgent incident fix.
  It works regardless of log size.
- P74 reduces how fast the durable log grows. With both, a long bridge session
  both bootstraps compactly (P73) and accumulates slowly (P74).
- P73 deliberately does **not** depend on P74: shipping P73 alone resolves the
  outage; P74 can land afterward without re-touching the bootstrap path.

## Acceptance Criteria

- `turn.planned` on the current event version records compact refs/revisions and
  no inline `LlmRequest`; older inlined events still decode and replay.
- The canonical `LlmRequest` for a planned turn is retrievable from CAS by
  `request_ref` for audit/debug/replay.
- A regression test with `T` turns over a stable `N`-entry active context shows
  durable-log growth linear in `T` (small events) rather than `~N·T`.
- The engine reducer remains deterministic and does not perform the CAS write
  itself; the runtime supplies `request_ref`.
