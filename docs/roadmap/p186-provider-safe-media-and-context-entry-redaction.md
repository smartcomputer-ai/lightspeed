# P186 — Provider-safe media and context entry redaction

**Status:** Proposed, 2026-10-01. Revises the request-time media rules of
[tool result media](p171-tool-result-media.md).

## Outcome

A session can recover when its active context no longer fits or is no longer
accepted by its provider, while preserving its durable history and as much
useful information as possible. Known media limits are handled before sending
the request. If a provider rejects a request for a reason the runtime cannot
predict, the failure says so plainly.
An operator can neutralize an offending entry with one public API call that
appends an ordinary event, instead of rewriting the session log.

Five changes deliver this, with provider-specific lowering and continuation:

1. Every image is sent as a normalized copy for the model, bounded in pixels
   and bytes. Stored originals are unchanged.
2. Each adapter checks the whole lowered request against its provider's
   request limits before sending it, and omits the oldest media when it would
   not fit.
3. A provider rejecting a request is reported as a distinct run failure,
   `RequestRejected`, carrying the provider's message word for word.
4. `session/context/redact` replaces the content of chosen entries with a
   fixed placeholder, in place, so an operator can neutralize the entry that
   causes a rejection.
5. A repair that invalidates preserved thinking uses the provider's supported
   continuation policy, so incompatible past reasoning does not itself prevent
   the session from continuing.

## Rationale: repair active context to preserve task continuity

Active context is a repairable projection of the session's durable history.
Compaction is one existing repair strategy: when context grows too large, it
summarizes older material so the task can continue. Media normalization,
request-time omission, and operator redaction address other reasons the
provider can no longer use the context. They share the same objective:
preserve the work already done and restore a usable conversation.

The choice of remedy depends on how confidently the runtime can identify the
problem. A known context or media limit permits an automatic repair. An
unexplained rejection calls for a visible failure and a precise operator
repair mechanism. The runtime does not choose entries to redact on a guess.

A repair preserves task continuity, but cannot promise identical reasoning
continuity. Compaction loses detail; omission hides older media; redaction
neutralizes selected content. If a repair invalidates provider-bound thinking,
the provider may also need to discard that reasoning. Losing it can require
the model to reconstruct conclusions or repeat analysis, but does not require
discarding the rest of the conversation or starting a new session.

Durable context repairs use revision guards, safe execution boundaries, and
ordinary audit events. Request-time transformations leave stored entries and
original blobs intact. Compaction and redaction retain their own policies and
implementations; this proposal does not introduce a generic repair framework.

## Incident

A long-running agent session accumulated 32 images through tool results,
one of them 2166 × 2464 px. Each image passed admission when it was produced.
Once a request carried more than 20 images, Anthropic applied its many-image
rule (every image at most 2000 px per side) and rejected the whole request
with `invalid_request_error`. Every later run resent the same history and
failed the same way, including the user's request to make the images smaller.
Recovery required restoring the session from a backup and rewriting its event
log by hand, because no public command could reach the offending entry.

The cause was specific to one provider, but the failure mode is not: any
provider can reject a history that grew past one of its limits or that holds
one entry it no longer accepts, and the runtime has no supported way out.

## Baseline

- [Tool result media](p171-tool-result-media.md) assumes providers downscale
  oversized images themselves and states that request failures are
  forbidden. The first part is true only per image: the many-image rule
  rejects rather than downscales, and it counts images from earlier turns.
- Admission (`engine::media::admit_tool_media`, gateway run input) checks media
  type, a 10 MB raw byte limit, and at most eight items per result or run.
  Dimensions, image counts across the session, and request totals are never
  checked.
- All three adapters read the original blob and base64-encode it on every
  request (`llm-runtime` `blob_io::read_base64`). The 10 MB raw admission
  limit therefore admits images over Anthropic's 10 MB *encoded* limit, and a
  few dozen ordinary images exceed the 32 MB request limit.
- Compaction requests lower the same entries, images included, so compaction
  fails on the same history.
- Provider errors are classified provider-neutrally (`ProviderFailureKind` in
  `llm-clients`), but `LlmRuntime` passes on only the retry decision. Every
  terminal provider error becomes an untyped message, and the run fails as a
  generic `ModelFailure`.
- Context ordering is entry-ID ordering: active entries keep strictly
  increasing `entry_id`, and a keyed upsert removes the old entry and appends
  its replacement at the tail. External context commands (`UpsertContext`,
  `ReplaceContextPrefix`, `RemoveContext`) address entries only by key.
  Run-appended entries have no key and are unreachable.
- Tool calls and tool results are separate entries (`ToolCall`, `ToolResult`),
  one per call. Tool-produced media are further separate user-role entries
  that follow the result.
- Every provider requires each tool call to be answered: Anthropic a
  `tool_result` per `tool_use`, OpenAI Responses a `function_call_output` per
  `function_call`, Chat Completions a `tool` message per `tool_calls` id.
  Removing calls is not safe either: an OpenAI Responses reasoning item must be
  followed by the item it produced, and Anthropic signs thinking across the
  assistant turn that holds the calls.

### Provider limits

The request limits check (Decision 2) keeps one row per provider API kind.
Slice 3 is complete only when every API kind has its row.

Anthropic Messages, first-party API, as of 2026-10-01:

| Limit | Value |
| --- | --- |
| Per image, any request | 8000 × 8000 px |
| Per image, request with more than 20 image blocks | 2000 px per side; earlier turns and `tool_result` images count |
| Images per request | 600 (100 for 200k-context models) |
| Per image, base64-encoded | 10 MB |
| Request body | 32 MB |
| Native resolution, high-resolution tier (Claude 4.7 and later) | 2576 px long edge, 4784 visual tokens; larger images are downscaled server-side |

OpenAI Responses and Chat Completions rows are taken from current provider
documentation during implementation.

## Decisions

### 1. Normalize every image once, with a fixed cap

The adapter sends a normalized copy for the model instead of the original
bytes. This is a property of lowering, not of admission or session state:
the stored blob, the entry's `ContentRef`, its `media:` handle, and every
projection keep referring to the original. A session may change models within
its provider, so a copy computed for one request shape must not be baked into
durable state.

- **The cap is fixed and independent of the request.** No side may exceed
  2000 px. A cap that depended on the request (for example, 2576 px until the
  21st image) would change the bytes of earlier images partway through a
  session, which invalidates the prompt cache and edits history as the
  provider sees it. With a fixed cap, an image is lowered identically from its
  first request to its last. On high-resolution models this gives up at most
  the 2000–2576 px range, which the provider would otherwise downscale itself.
- **A byte budget per image.** If a resized image is still above the
  per-image budget (initially 3.75 MB raw, which is 5 MB encoded), it is
  re-encoded as JPEG at a fixed quality, with any alpha channel flattened
  onto white.
- **Compliant images pass through byte-identical.** Dimensions come from the
  image header without a full decode. An image within both the pixel cap and
  the byte budget is sent unchanged, so existing sessions without oversized
  images see no prompt-cache change.
- **Output is deterministic.** For a given source and normalization spec, the
  output bytes are always the same: fixed resampling filter, fixed encoder
  settings, pinned codec crate version. Caching is therefore only an
  optimization. A worker-local LRU keyed by (source blob ref, spec version)
  avoids repeated decodes; a cache miss on another worker yields the same
  bytes. A persistent copy store is added only if measurement calls for it.
- **GIF and animated images** lower their first frame, which matches what
  providers read.
- **PDFs are not normalized.** They count toward request totals in Decision 2.
- One shared function in `llm-runtime` serves all three adapters and the
  compaction request path, which shares lowering. The engine is unchanged.

Normalizing an existing history can change image bytes the provider has
already seen. This migration uses the thinking-continuation policy in Decision
5, just as omission and redaction do.

### 2. Check the whole request against provider limits

After lowering, each adapter compares the request with its row of the limits
table: image block count, per-image encoded bytes, and total body bytes.

- **When it fits, nothing changes.**
- **When it does not fit, the oldest media is omitted.** Media entries are
  replaced, oldest first, with the placeholder text the text-only path already
  uses:
  `[image · media:3f9a2c1d4e7b · omitted from this request to stay within provider limits]`.
  Media from the current run's input and from the latest tool batch is
  protected: it is omitted only after all older media, because the model must
  see what it just asked for.
- **Protected media degrades newest-first.** Per-item admission does not bound
  the aggregate request: eight images at the 5 MB encoded budget already
  exceed a 32 MB body limit, even in a single result, and documents and
  parallel tool batches can exceed it too. When the protected media alone
  does not fit, its newest items are kept and the rest receive the same
  placeholder. Failing the turn instead would not help: the latest tool batch
  stays the latest after the run fails, so every later request would fail the
  same way.
- **The cut point moves in steps.** Which media is omitted is a pure function
  of the context, so retries produce identical requests. The boundary moves
  only when the request crosses the limit, and then it drops to a low-water
  mark well below the limit, so a growing session invalidates its prompt
  cache rarely rather than every turn.
- **The check never fails a request.** It manages media only, and a single
  image always fits within the per-image budget. A request whose non-media
  content alone exceeds the body limit is a context-size problem for
  compaction, or a rejection under Decision 3.

Omission rewrites earlier user content as the provider sees it, in sessions
that are still healthy. Decision 1 keeps it rare, because pixel limits no
longer trigger it; only request totals do.

### 3. Report provider rejections as `RequestRejected`

A terminal provider error classified as `InvalidRequest` or `ContextLength`
fails the run with a new `RunFailureKind::RequestRejected`. Every other terminal
provider error stays `ModelFailure`.

- The classification comes from the existing provider-neutral
  `ProviderFailureKind`. The LLM I/O boundary gains a rejected outcome beside
  `Failed`, the turn failure records it, and the run failure carries it. The
  engine stays deterministic: it records the classification it is given and
  never inspects provider errors.
- The failure record keeps the provider's message word for word.
- Clients show that the provider rejected the request, with that message.
  They do not suggest a fix or guess which entry caused it: the runtime cannot
  know, and provider messages differ in how precisely they point at a cause.
- The public run failure view gains the new kind; the API contract and the
  TypeScript consumers are regenerated.

### 4. Redact context entries in place

`session/context/redact { sessionId, entryIds }` replaces the content of each
named entry with a fixed placeholder chosen by the engine. The engine gains a
`RedactContextEntries { expected_revision, entry_ids }` command and an
`EntriesRedacted { base_revision, entry_ids, reason }` context event.

**In place, not removal.** A redacted entry keeps its entry ID, position, kind,
role, and `call_id`. Removing a tool result would leave its call unanswered,
which every provider rejects; removing the call as well breaks reasoning and
thinking that providers bind to it. Swapping content under the same entry ID
keeps both the ordering invariant and call/result pairing. A keyed upsert
cannot do this, because it appends a new entry at the tail.
Pairing alone does not preserve thinking bound to the earlier content;
Decision 5 supplies the continuation policy after that content changes.

**The engine chooses the placeholder.** Clients name entries; they never supply
replacement content, so this is a repair operation, not a general
context-editing API.

| Entry | Placeholder |
| --- | --- |
| Tool result | `[tool result removed by operator]` |
| Media (image or document) | `[image · media:3f9a2c1d4e7b · removed by operator]` |
| User message (run input, steering, context edit) | `[message removed by operator]` |

Rejected, request-level:

- tool calls, assistant output, reasoning, and provider-opaque entries, whose
  content is provider-signed or provider-shaped;
- any redaction while a run is active, or while compaction is pending;
- unconsumed run input or steering, by the existing guard.

An ID that is not in active context, or is already redacted, reports `absent`,
so retries are idempotent. The response reports a result per ID.

The original content stays in the event log. The web transcript resolves
`media:` handles from transcript history, so a redacted image still renders
there, beside the redaction event. A sub-agent hand-off resolves links against
the child's active context, so a redacted image no longer travels with it.

Redaction is available through the API and the runtime CLI. There is no web
affordance and no automatic redaction.

### 5. Continue after a repair invalidates preserved thinking

Thinking compatibility is part of recovery for normalization of existing
histories, omission, redaction, and compaction that retains thinking from
earlier turns. It is an implementation requirement, not an optional check on
sessions that are still healthy.

The Anthropic adapter defaults to
`thinking.block_binding.prefix_mismatch_behavior: "drop_block"` wherever the
model and thinking mode support it, with the
`thinking-binding-controls-2026-08-01` beta header. Anthropic then drops
incompatible thinking and subsequent thinking blocks while retaining the
other request content. This policy remains on subsequent requests and after
restart; it is not a one-request retry setting. See the provider's
[preserved-thinking contract](https://platform.claude.com/docs/en/build-with-claude/preserved-thinking).

Setting the field also changes behavior on accounts the provider does not
enforce by default (created before 2026-08-31): any value opts the request into
enforcement, so mismatched thinking that such accounts currently pass to the
model is dropped instead. This is the intended contract, and it makes every
deployment behave the same.

The adapter continues sending the stored thinking unchanged. Original
reasoning stays in the event history. New responses may still generate
thinking. Other request errors continue through Decision 3; this policy does
not make every provider rejection recoverable.

`drop_block` also absorbs history edits the runtime makes by mistake, which
would otherwise surface as rejections. Two measures keep such bugs visible:

- Production logs every reported `input_transformations` entry with its path
  and reason, and exports a count of dropped blocks per session, so unexpected
  drops are observable.
- Live and CI suites that do not exercise a repair send `"error"`, so an
  unintended history edit fails a test instead of being absorbed.

Models or modes that cannot use this policy need a separately tested fallback
that durably excludes affected historical thinking from future requests,
without gaps or later reintroduction. The exclusion must survive restart and
preserve a valid tool sequence. Recovery support for such a mode is not
complete until that path is verified. Anthropic accepts `block_binding` only
with `adaptive` and `enabled` thinking, so this applies today to `disabled`,
which the adapter sends for reasoning effort `none`, and would apply to
`between_tools` if the adapter adopts it. Other adapters follow their native
contracts; Anthropic's policy does not authorize stripping OpenAI reasoning
items or other provider-opaque content.

Provider wire settings, their interpretation, and dropped-block diagnostics
remain in the adapter. This decision does not change the engine or the
public contract, and nothing inspects signatures. Stable normalization and
stepped omission still matter: fewer history edits preserve more reasoning
and more of the prompt cache.

## How a failing session recovers

| Failure | Handled by | Automatic |
| --- | --- | --- |
| Context exceeds its token budget | Existing compaction policy: older context summarized | According to session policy |
| Image over a provider's pixel or per-image byte limit | Normalization (Decision 1): a bounded copy is sent | Yes |
| Request over a provider's image count or total size | Request limits check (Decision 2): oldest media omitted, protected media last and newest-first | Yes |
| Unexplained rejection caused by a redactable entry | `RequestRejected` (Decision 3), then redaction by an operator (Decision 4) | No |
| A repair invalidates preserved thinking | Provider-specific continuation (Decision 5): incompatible past thinking excluded from model input | For verified model and mode combinations |

The runtime repairs automatically only what it can predict. For anything
else it cannot know which entry is at fault, and removing context on a guess
is worse than a visible failure. The provider's message, shown word for word,
is the operator's starting point.
Redaction is a repair mechanism for selected entries, not a guarantee that
every rejection is caused by content it can repair.

## Slices

1. **Thinking continuation.** Anthropic adapter defaults, beta header,
   dropped-block logging and counts, `"error"` in suites that do not exercise a
   repair; verified fallback for `disabled` thinking before claiming recovery
   support there. Tests: unchanged histories retain valid thinking;
   existing-image normalization, omission, redaction, and compaction that
   retains thinking continue with prefix enforcement enabled; later turns and
   a restart continue; original history is unchanged. A credentialed live
   suite exercises an enforcing model and mode explicitly, rather than relying
   on the account age or the default model.
2. **Normalization.** Shared lowering function in `llm-runtime`, header probe,
   fixed cap, byte budget, deterministic encoding, worker-local cache, all three
   adapters. Tests: oversized PNG and JPEG are downscaled within the cap;
   compliant images pass through byte-identical; output is identical across
   calls; a 32-image history with a 2166 × 2464 image lowers within the
   many-image rule.
3. **Request limits check.** Limits rows for every API kind, stepped omission,
   newest-first degradation of protected media. Tests: omission order;
   protected media omitted last; an eight-image result over the body limit
   keeps its newest images, and the next request is identical; identical
   requests across retries; the cut point holds steady while under the limit.
4. **Rejection and redaction.** `RequestRejected` through the I/O boundary,
   turn, and run failure; the redaction command, event, and placeholders;
   `session/context/redact`; CLI support; contract regeneration; replay vectors
   for redaction, its rejections, and a redacted tool result lowering as its
   placeholder with pairing intact on every adapter.

Slice 1 comes first because every later slice can change content the provider
has already seen; repair is complete only when the session can continue after
the change. Slice 2 addresses the incident's per-image limit. Slice 4 supplies
operator repair for unexplained rejections caused by redactable entries.

## Non-goals

- Changing compaction triggers or summarization strategy. Compaction inherits
  the request safeguards and applicable thinking-continuation policy.
- Introducing a generic context-repair framework.
- Suggesting fixes in clients, or redacting automatically after a rejection.
- Retrying after a rejection by matching provider error text.
- Removing tool calls or call/result pairs, and redacting tool calls.
- Client-supplied replacement content.
- Storing copies for a specific provider in session state, or rewriting
  existing events.
- The Anthropic Files API as an alternative to base64 payloads.

## Open questions

- **Announcing the resize.** Whether a normalized image's announcement should
  state the dimensions the model sees (for example, `· shown at 2000×1400 of
  4000×2800`) to support coordinate-based work. It is deterministic, so it
  does not affect caching.
