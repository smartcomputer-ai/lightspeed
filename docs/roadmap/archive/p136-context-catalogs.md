# P136 — Context Catalogs: Refresh Without Rewriting the Prefix

**Status**

- Proposed 2026-08-26, surfaced by the P135 review question "where does a
  refreshed catalog land in the context, and does it invalidate the prompt
  cache?" — it does, from the front. Core tier (`harness`, `llm-runtime`,
  `temporal-runtime` projection, `api` views). Not yet sliced for a sprint;
  the design is settled enough to build. Implemented the same day; see
  the entry below.
- Extended the same day at Lukas's request: the supersede mechanism is
  exposed to clients as an external catalog (`InputItem::Catalog` on
  `session/context/append`), so bots and other platform controllers get
  cache-preserving menus through the API. P135's bot directory is its first
  consumer.
- **Implemented 2026-08-26** (all four slices, one pass):
  - Engine: `ContextEntry.supersedes`; `ContextEntryKind::Catalog { title }`;
    catalog kinds supersede on keyed apply (`supersede_target` validated in
    the reducer), `SUPERSEDED_CATALOG_CAP = 4` oldest-first;
    `current_context_entry` / `is_superseded_context_entry` helpers, every
    "current entry for key" lookup (workflow, gateway, tools) now takes the
    newest entry; superseded versions are compactable and prunable, current
    ones are not; `RemoveContext` clears every version; the planner renders
    **all** entries in id order after the instructions — which also fixed a
    P134 defect: the sub-agent catalog was excluded from the planned
    snapshot and never reached the model.
  - Runtime: `catalog_prompts` prefixes the successor with
    `CATALOG_UPDATE_HEADER`; the earlier version renders unchanged (asserted
    by byte-level prefix tests on the Anthropic and Responses adapters);
    `Catalog` renders as `<title>:` plus the text.
  - API: `InputItem::Catalog { title, text }` (context-only; run input and
    the CLI reject it), `ContextEntryKindView::Catalog`, `supersedes` /
    `supersededBy` on `ContextEntryView` (`supersededBy` on state views
    only); contracts, TS client, Configurator tools regenerated; the web
    transcript dims superseded catalog versions.
  - Superseded versions are sent to the compactor like conversation and
    then pruned; a small, deliberate simplification over "dropped for free".
- Builds on P95 (sparse config, derived toolset), P107/P113 (VFS catalog,
  skills and prompts as VFS roots), P134 (the sub-agent catalog, whose
  "menu is a refreshed catalog context entry" rule this doc keeps).
  Complements [P137](p137-prompt-caching.md): P137 makes the prefix cache
  exist on every provider; P136 stops catalog refreshes from breaking it.
- Greenfield: reducer and view changes ship without event back-compat; dev
  databases reset.

## What a catalog is

A catalog is a context entry that tells the model *what it may pick from*.
It is rebuilt by the runtime from configuration plus external state (VFS
roots, profile records), keyed, content-fingerprinted, and refreshed on
its own — which is what distinguishes it from the two other things the
model sees:

- **Tool schemas**: derived only at session start, config put, and profile
  apply; never refreshed on their own (the remote MCP tool list has exactly
  that staleness). Frozen by design so bindings can be immutable.
- **Instructions**: the system prompt. Replaced whole through
  `ReplaceContextPrefix` on the `instructions` key prefix; a change there
  rewrites the front of the prompt by definition and is out of scope here.

## Inventory: where catalogs are used

| Kind | Key | Built from | Grant | Rendered as |
|---|---|---|---|---|
| `VfsCatalog` | `environment.vfs_catalog` | resolved `features.vfs.workspaceLinks` | `features.vfs` | user text (Anthropic) / developer message (OpenAI) |
| `SkillCatalog` | `skills.catalog.vfs` | skill roots under `features.vfs.skills.roots` | `features.vfs.skills` | same; activations append `SkillActivation` entries |
| `SubagentCatalog` | `subagents.catalog` | `features.subagents.agents[]` ⨝ current profile records | `features.subagents` | same |

Adjacent, refreshed by the same activity but not catalogs: VFS **prompts**
become `instructions.*` entries (system prompt). At the platform tier, the
P135 **bot directory** is a catalog owned by an external controller; it
has no kind of its own today and is the first consumer of the external
catalog proposed below.

**Publication sites** (two, same commands):

1. `runtime_projection_refresh` — run by the session workflow immediately
   before admitting a `RequestRun` on an idle session with nothing queued
   (`should_refresh_runtime_projection_before_admitting`). It resolves
   workspace links, rebuilds each enabled catalog, writes the snapshot to
   CAS, and compares the ref with the active entry's ref: unchanged →
   nothing; changed → `UpsertContext { key, entry }`; grant revoked →
   `RemoveContext { key }`.
2. The gateway's idle-read path (`load_session_state_with_current_run_context`):
   an idle `session/read` (and the prompt/skill/sub-agent read methods)
   refreshes environment → prompts → skills → sub-agents in sequence, so a
   client always sees the current catalogs. A *read* can therefore bump
   the context revision.

**Reducer treatment.** Catalogs and instructions are configuration, not
conversation: `compactable_context_entry_ids` excludes them, provider
compaction may not prune them (`is_provider_compaction_prunable_entry`
returns false), and the key → kind pairing is validated at admission (a
catalog key may only carry its own kind).

**Rendering.** Every adapter reads the JSON snapshot from CAS at request
time and renders text (`skill_catalog_text`, `subagent_catalog_text`,
`vfs_catalog_text`) as one message at the entry's position in the message
list. Catalogs are first published at the first run admission, so their
entry ids come right after the instructions: they sit at the **front** of
the message list, before the first user message.

## The problem

`UpsertContext` is a keyed `EntriesApplied`, and the reducer implements a
keyed replace as **remove the previous entry wherever it is, then push the
new one at the tail with a fresh id** (`apply_entries_applied`,
`crates/harness/src/core/components/context.rs`). Two consequences:

1. **The prefix moves.** Removing a message from the front shifts every
   byte after it. Provider prompt caches are prefix caches (Anthropic at
   explicit breakpoints, OpenAI automatically), so one catalog change
   costs an uncached read of essentially the whole session. On a short
   interactive session that is rare and cheap. On a managed session that
   lives for months — a bot's main session, a keyed per-incident session —
   every profile description edit, every skill added to a VFS root, every
   `features.subagents` change, and every idle-read refresh that happens to
   find a change, re-reads the whole history at full price. The fingerprint
   check keeps the *frequency* low; it does nothing about the *cost* of
   each occurrence.
2. **The catalog's position is inconsistent.** An unchanged catalog stays
   at the front; a changed one jumps to the tail. Harmless for the model,
   but it means the "same" entry is sometimes the oldest and sometimes the
   newest message, and a debugging view has to explain why.

Instructions rewrites and compaction also rewrite the prefix; both are
deliberate, rare, and inherent. Catalogs are the one prefix mutation that
is neither deliberate nor rare on long-lived sessions.

## Proposed solution: append-with-supersede

Keep the P134 rule (menus are refreshed catalog entries, never tool
schema) and change what a keyed replace *does* for catalog kinds:

- **The old entry stays.** For supersedable kinds — the three catalogs,
  a property of `ContextEntryKind` — a keyed `EntriesApplied` appends the
  new entry and marks the previous one `superseded_by: <new entry id>`
  instead of removing it. The superseded entry keeps rendering
  **byte-for-byte unchanged** (no annotation; annotating would move the
  bytes), so everything before the new entry is a stable prefix.
- **The new entry says so.** Its rendered text carries a first line such
  as "Sub-agent catalog (updated — supersedes the earlier catalog)". The
  model reads the latest as authoritative; the header is part of the new
  entry, at the tail, where a change belongs anyway.
- **Superseded entries are the first thing compaction drops.** They join
  `compactable_context_entry_ids` and become provider-prunable; the next
  prefix rewrite (compaction, prune, `RemoveContext`) removes them for
  free. Until then they cost their own tokens once per request — catalogs
  are a few hundred tokens, and a rewrite of the whole prefix is what they
  are saving.
- **Bounded.** At most `SUPERSEDED_CATALOG_CAP` (4) superseded entries per
  key; beyond it the oldest is removed — one prefix invalidation per four
  changes instead of one per change, for a catalog that churns.
- **Lookups follow the current entry.** `active_context_ref(key, kind)`
  and the gateway's `active_*_catalog_ref` return the non-superseded
  entry; `RemoveContext { key }` removes every entry under the key.
- **Views.** `ContextEntryView` gains `supersededBy?`; the web context view
  greys superseded entries and links them to their successor.
- **Unchanged.** Instructions keep immediate replacement (the system prompt
  is rewritten either way). Client keyed appends (`session/context/append`)
  keep today's semantics: those keys are idempotency handles; a client
  that wants supersede semantics sends a `Catalog` item (below). Tool
  schemas stay frozen.

### External catalogs: the same mechanism through the API

Controllers outside the core have the same need — a bot's directory of
addressable bots (P135), a Channels participant roster, an adapter's menu
of remote agents — and today they can only choose between a keyed append
(rewrites the prefix) and a fresh key per snapshot (never rewrites, but
accumulates and asks the model to fold versions). Expose the supersede
semantics instead:

- **API.** `InputItem::Catalog { title, text }`, accepted only on
  `session/context/append` — run input rejects it with a typed
  `InputAdmissionFailureKind` (catalogs are context, not conversation).
  The key is a client key (`bot:directory`), validated like any external
  key (no `run` prefix, none of the runtime catalog keys or the
  `instructions.` / `skills.activation.` prefixes). A same-content put is
  `Unchanged`, as for any keyed append (content-addressed ref compare).
- **Engine.** `ContextEntryKind::Catalog { title }`, client-owned and
  opaque, joins the supersedable kinds: the previous version stays
  rendered byte-for-byte, the new one lands at the tail, superseded copies
  compact first, the cap applies per key, `session/context/remove` clears
  every version under the key. The current version is non-compactable and
  non-prunable like the core catalogs, so it survives compaction on its
  own — an external controller never needs a "republish after compaction"
  path.
- **Rendering.** Title line plus text, in the same role and position as
  the core catalogs; a successor's title line reads "<title> (updated —
  supersedes the earlier version)".
- **Views.** `ContextEntryView.kind = catalog { title }` with
  `supersededBy?`; the web context view treats it like the core catalogs.
- **Busy sessions.** Context mutations wait for the turn boundary (the
  P129 rule); a controller that puts a catalog while a turn is in flight
  sees it land at the next boundary, as it does for keyed appends today.
- **What it is for.** Menus and rosters — "what the model may pick from"
  — that change rarely relative to turns. Not volatile state (a bot's
  buffers and budget stay behind `bot_status`) and not conversation.

The core catalogs stay typed kinds: `skills/list` and the sub-agent API
read their JSON snapshots back, which an opaque document cannot serve.
The first consumer is P135's `bot:directory`; it replaces that doc's
snapshot-plus-deltas design outright.

### Alternatives considered

- **Deltas instead of snapshots** ("agent `x` added"): smaller, but needs
  diff logic per catalog kind and asks the model to fold a stream. Core
  catalogs are small; a fresh snapshot is simpler and more robust. Deltas
  stay the right shape at the platform tier for the bot directory, where
  changes are already events.
- **Catalogs in the system prompt**: any change invalidates 100 % of the
  prefix, and the system prompt is the one thing that must not churn.
- **Refresh only at prefix-rewrite boundaries** (compaction): removes the
  problem by never refreshing mid-history, but a profile description edit
  would not land for hours or days. Kept only as the point where superseded
  entries are dropped.
- **Move catalogs to the tail on first publication**: does not help — the
  removal is what shifts the prefix, wherever the entry sits.

## Slices

1. **Engine** (1 d): `superseded_by` on `ContextEntry`; supersedable kinds;
   the keyed-apply branch, the cap, compactable/prunable inclusion,
   `RemoveContext` under a key removes all; reducer tests.
2. **Runtime + projection** (½ d): "supersedes" header in the three
   adapters' catalog renderers; `active_context_ref` and the gateway
   `active_*_catalog_ref` follow the current entry; a deterministic
   **byte-level prefix test** in `llm-runtime`: materialize request N,
   upsert a catalog, materialize request N+1, assert N's message list is a
   prefix of N+1's — no provider needed.
3. **API + web** (½ d): `supersededBy?` on `ContextEntryView`, contract
   regenerated, context view rendering.
4. **External catalogs** (1 d): `InputItem::Catalog`, the context-only
   admission rule, `ContextEntryKind::Catalog { title }` in the
   supersedable set, external-key validation, the three renderers,
   `ContextEntryView` kind, contract and TS client regenerated, the web
   context view. Unblocks P135 slice 1.
5. **Live** (with P137): the catalog-change scenario keeps the cache hit
   rate on both providers, for a core catalog and for an external one.

## Tests

- **Engine**: upsert of a catalog kind keeps the old entry active and
  marks it superseded; a second upsert chains; the cap removes the oldest;
  compaction drops superseded entries before anything else; `RemoveContext`
  clears the key; instructions still replace immediately; a client keyed
  append still replaces.
- **Runtime**: prefix-stability bytes test above, for each adapter; the
  rendered header appears on the successor only.
- **Gateway**: idle-read refresh after a profile description edit produces
  one superseded entry and one current entry; `session/read` reports both.
- **External catalogs**: a `Catalog` item on run input is rejected with a
  typed failure; a put on a reserved or runtime key is rejected; a put with
  the same content is `Unchanged`; a changed put supersedes and renders the
  header; the current version survives compaction.
- **Live** (P137): hit rate holds across a catalog change, core and
  external.

## Non-goals

- Changing keyed-append semantics for client keys.
- Refreshing tool schemas in place (P100's deferred declaration
  evolution; separate decision).
- Any change to instructions/system-prompt replacement.
- Making the core catalogs generic: their typed snapshots have readers
  (`skills/list`, the sub-agent API); `Catalog` is for opaque client
  documents.
