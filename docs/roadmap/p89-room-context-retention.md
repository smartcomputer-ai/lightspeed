# P89: Room Context Retention And Compaction

**Status**
- Proposed 2026-07-02.
- Phase 1 implemented 2026-07-02: `context/remove` API method (batch, per-key
  `removed`/`absent`/`failed` results, reserved `run.` prefix rejected at both
  gateway and engine admission), hierarchical bridge room keys
  (`channel.room.<room>.msg.<id>`), and watermarked bridge pruning of the
  unconsumed backlog (`BRIDGE_ROOM_RETENTION_HIGH`/`LOW`, prune-when-idle,
  ≤64-key chunks, backlog cleared when any run starts in the conversation).
  The engine consumed-entry high-water mark was deferred: the bridge
  approximates the consumption boundary by its own run submissions, which is
  exact for its sessions.
- Phases 2-3 (summarize-and-replace, budget-aware policy, compaction-trigger
  verification for always-on sessions) remain open.
- Follows **P88 (Media-Aware Context Append And Activation)**. P88 made group
  ingestion eager: every allowed room message is committed as model-visible
  context whether or not it triggers a run.

## Problem

Eager ingest changes the economics of a busy group chat. Before P88, session
context grew per mention; now it grows per message. An always-on group agent
accumulates an unbounded transcript of room chatter:

- token cost and latency of every run grow with room traffic, not task size;
- old chatter crowds the model's attention long after it stops being useful;
- the generic `context/compact` pass has no notion of "room history", so it
  cannot apply channel-appropriate policy (keep recent verbatim, summarize the
  rest, never touch instructions or task context).

## Design Position

Two different lifecycles, two different mechanisms:

- **Consumed history** — entries that have been included in at least one
  planned turn are conversation history. Their lifecycle belongs to normal
  compaction (`context/compact` and the session compaction policy), which
  summarizes rather than drops. Retention must never prune them; drop-oldest
  on messages a run has already incorporated would silently rewrite the
  model's memory of a conversation it took part in.
- **Unconsumed backlog** — entries appended since the last turn (room chatter
  no run has ever seen) are an ingestion buffer, not history. This is what
  grows without bound in an always-on room — in `silent` mode nothing is ever
  consumed at all — and it is the only thing this item prunes. Dropping the
  oldest unconsumed chatter loses information the model never used, which is
  the cheapest possible loss.

The two mechanisms meet in the middle: in active rooms, runs continually move
chatter from backlog to history and compaction bounds the history; in quiet or
silent rooms, backlog retention does all the work. Part of this item is
verifying the compaction policy actually triggers for always-on group
sessions (token-threshold policy on session config), since retention alone no
longer bounds consumed history.

Retention itself is a channel/bridge policy applied through generic context
primitives, mirroring the P88 split: the hosted API stays channel-neutral, the
bridge decides what a room needs.

Target model:

```text
channel.room.<room>.msg.<message>.text     recent messages, verbatim
channel.room.<room>.msg.<message>.media.N  recent media/transcripts
channel.room.<room>.summary                one rolling summary of pruned span
```

## Low-Lift First Cut (Phase 1: watermarked drop-oldest)

No LLM calls, no engine changes, no new stores.

**Prune by removal, not replacement.** `ReplaceContextPrefix` removes every
entry under the prefix and re-applies the replacement set as new entries: the
retained messages get new entry ids and move to the end of active context,
behind assistant replies they originally preceded. Ordering scrambles and the
provider prompt cache is fully invalidated on every prune. Retention must
therefore remove only the evicted keys and leave retained entries untouched in
place. Removal keeps retained entry ids, positions, and the assistant-reply
interleaving intact; the cache invalidates only from the oldest removed
position, which is unavoidable for any pruning scheme.

1. **Hierarchical room keys.** Change the bridge key scheme from
   `channel.room.<hash(provider,account,conversation,message)>` (message id
   baked into the hash, so a room has no shared prefix) to
   `channel.room.<hash(provider,account,conversation)>.msg.<messageId>`.
   Room-scoped bookkeeping becomes possible; per-message keys stay stable for
   append idempotency.
2. **Expose `context/remove`.** New API method mapping to the existing
   `RemoveContext` engine command, accepting a key batch and submitted as one
   admission signal so a prune is atomic in the admission queue. Validation,
   reserved-prefix checks, and admission ordering already exist.
3. **Consumption boundary.** Retention prunes only entries no turn has ever
   seen. The engine already computes exactly which entry ids enter each
   planned request (`planned_context_entry_ids`, applied in
   `mark_current_context_consumed_by_turn`) but persists consumption only for
   run-input and steering batches. Add a single state field — the high-water
   context entry id included in any planned turn — updated at the existing
   mark-consumed point and exposed through session status. Entries at or
   below the mark are history (compaction's territory); above it are prunable
   backlog. Fallback if the engine addition is deferred: the bridge
   approximates the boundary by run boundaries it initiated itself (keys
   appended since it last triggered a run in that room), which is correct as
   long as the bridge is the session's only client.
4. **Watermarked bridge pruning.** The bridge tracks appended message keys per
   room (it already receives them in every append response). Pruning uses two
   watermarks over the *unconsumed backlog*: when it exceeds `HIGH` (e.g. 300)
   messages, remove the oldest down to `LOW` (e.g. 200) in one batch. Between
   prunes the room span is append-only, so the prompt cache stays warm; the
   cost of one invalidation is amortized over `HIGH - LOW` messages instead
   of paid per message, which a naive "cap at N" ring buffer would do.
5. **Prune only when idle.** The bridge prunes between runs, never while a run
   is active or queued in that session. This also guarantees a prune can never
   remove the trigger entries of an in-flight context-triggered run (the
   engine protects unconsumed run *input* entries from removal, but
   context-run trigger entries are ordinary keyed entries and are not
   engine-protected).

Nothing is lost from the record: removal only changes what future turns see.
The durable session log keeps every appended entry, and completed run views
attribute their context by entry id from the log, so audit and projections of
past runs are unaffected by later pruning. The assistant's own replies
(separate, non-`channel.room.*` entries) are never touched by room retention
at all.

Cost: one new API method plus bridge bookkeeping. This caps context growth per
room and is enough to run always-on groups safely.

## Phase 2: Summarize-And-Replace

Replace the pruned backlog span with meaning instead of dropping it (consumed
history already gets this from normal compaction):

- before pruning, generate a summary of the outgoing span (worker activity or
  a dedicated run, never inside `engine`);
- the prune commits one updated `channel.room.<room>.summary` entry alongside
  the batch removal;
- summary updates fold the previous summary with the newly pruned span so the
  rolling summary stays bounded.

Open ordering question: upserting an existing key re-materializes it at the
end of active context, so a bridge-written rolling summary would sit *after*
the retained verbatim tail ("summary of earlier discussion" below newer
messages). Options, to be decided in this item: accept trailing placement with
a self-describing header; add an engine-level notion of entry placement for
summaries; or route Phase 2 through the existing engine compaction machinery,
which already owns reordering during summarization. Do not build a fourth
ad-hoc ordering mechanism in the bridge.

Failure policy: if summarization fails, fall back to Phase 1 drop-oldest;
never block ingestion on the summarizer.

## Phase 3 (open): Budget-Aware Policy

- token budgets per room instead of message counts, using entry token
  estimates already carried by context entries;
- retention config surfaced per binding (`/retention` control command or
  profile field);
- room-aware compaction policy: teach the generic compaction summarizer to
  treat `channel.room.*` history as chatter (aggressively summarizable) versus
  task context (conservatively preserved), so consumed history compaction and
  backlog retention converge on one coherent room memory.

## Non-Goals

- No engine-side channel awareness. `engine` keeps generic keyed context; all
  room semantics stay in bridge policy.
- No per-message TTL scheduling; retention triggers on append, not timers.
- No cross-room/global summarization in this item.

## Testing Requirements (Phase 1)

- `context/remove` rejects reserved prefixes (`run.`, instructions/catalog
  keys), removes a key batch atomically, and is idempotent on retry (removing
  an already-absent key is a per-key no-op, not a call failure).
- Retained entries are untouched by a prune: same entry ids, same relative
  order, interleaving with assistant/run entries preserved (this is the
  prompt-cache guarantee — assert entry identity, not just presence).
- Consumption boundary: a prune never removes an entry that was included in
  any planned turn — append chatter, run a turn over it, append more, prune:
  only the post-turn backlog is eligible, regardless of watermark pressure.
- Watermark behavior: no prune below `HIGH`; a prune removes exactly down to
  `LOW`, oldest first; the span between prunes is append-only.
- Pruning never runs while the session has an active or queued run, and never
  removes the summary key.
- A run triggered from a context key that was pruned before `run/start` is
  rejected with the existing missing-trigger-key admission error (bridge must
  trigger before pruning the triggering message; test the ordering).
