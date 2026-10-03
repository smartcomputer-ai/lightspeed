# P135 — Bot Federation: Events Between Bots

**Status**

- Proposed 2026-08-26, recommendation-first design. **Implemented
  2026-08-26**: slices 1–3 (identity and ids, bus, receipts) all landed the
  same day; see the slice notes for what shipped and the two deliberate
  deviations (poll filter misses stay unarchived; a `filtered` refusal
  leaves the archived copy at the receiver for replay).
- Three revisions the same day, each after review:
  1. The first draft allowed bot → bot *configuration* behind a `manage`
     grant while deferring *creation*. Lukas asked why one and not the
     other; the asymmetry does not hold (a trigger put on a neighbour is as
     durable as a new bot). Adopted: **no cross-bot authority** — bot ↔ bot
     is events only; authority over a bot belongs to the bot itself
     (`selfConfig`) and to humans. The `manage` alternative is recorded in
     [later/pNNN-bot-manage-grant.md](later/pNNN-bot-manage-grant.md).
  2. "How does A learn of B, and is it cache-safe?" Answered by a
     `bot:directory` catalog published through
     [P136](p136-context-catalogs.md)'s external catalog (supersede at the
     tail, never a prefix rewrite). P136 and [P137](p137-prompt-caching.md)
     are done; nothing blocks this item.
  3. An outside review of the wire contract found four real defects
     (verified against the code): addressed fan-out over several matching
     triggers collides with the `(bot_id, event_id)` uniqueness index and is
     silently deduped; `bot_emit` is `accepted-push`, so no typed refusal
     ever reaches the model — true today for the self-emit rate cap; the
     proposed deterministic reply reported "finished" for `append`/`steer`
     deliveries, which finish before any run; and the return address was a
     concrete session id although routed sessions rotate generations from a
     logical base. Adopted, simpler than the review proposed: **one inbox
     trigger per bot, one event per addressed emit, joined admission, no
     automatic replies in v1**. Resolution receipts are slice 2 with a
     logical return route; publish/subscribe is deferred until a use case
     asks for fan-out.
  4. Bot-side model-facing ids folded in from [P138](p138-model-facing-ids.md),
     which stays core-only. Bots are already P138's good exemplar (`#N`,
     names); what remains is subtractive — §8 — and ships as its own first
     slice, independent of federation.
  5. Same day, from Lukas: separate bot *id* from bot *name*, as the
     platform does everywhere else. First answer was uuid identity plus a
     mutable name; Lukas asked why not an authored, readable id like a
     session or profile id. That is the platform's real pattern — an
     authored immutable id plus a mutable display name — and today's bot
     `name` already *is* the authored id; the uuid the API and UI address
     bots by is the odd one out. §9; part of slice 1.
- Absorbs P131 workstream 6 (bot → bot events) and the "Bot federation"
  note in `later/pNNN-fleet-vs-bots.md`. What else "make bots really good"
  means, unrelated to federation, is in
  [later/pNNN-bots-beyond-federation.md](later/pNNN-bots-beyond-federation.md).
- Builds on P130 (controller, store-then-wake admission, `#N` events, CEL
  filters, routing, coalescing, breakers, `selfConfig` / `selfEmit`) and
  P134 (durable orchestration above, attached delegation below; provenance
  not ownership; deterministic identities; no new transports).
- Greenfield: no wire, schema, or event back-compat; `bot:self` and
  `self-<uuid>` event ids go away.

## Why

After P134 there are exactly two ways to get more than one session's worth
of work done, and they do not overlap: **attached delegation** (a run fans
out to one-shot children over promises and joins them inside its own loop —
vertical, synchronous for the model, bounded by tree limits) and **durable
orchestration** (a deterministic controller admits, filters, routes,
coalesces, budgets, and drives managed sessions for months — horizontal,
asynchronous, bounded by budgets and breakers).

The second stops at the edge of one bot. A bot can receive from the world
and emit to itself, but cannot address another bot or hear from one, so a
team — triage hands incidents to infra, comms narrates what the others
decide — can only be wired through webhooks by hand.

What keeps this small is what kept P134 small: **do not add a transport**.
Bots already speak one protocol to the world, an event through admission.
Bot ↔ bot is that same event, admitted by the receiver's own trigger, plus
a loop bound. And no authority: no bot configures, enables, or creates
another. The deterministic layer (breakers, budgets, auto-disable) is the
manager; a bot that wants a neighbour to change asks, and the neighbour's
human-written brief decides.

## The shape

```text
   ┌──────────────┐   bot_emit { to: "b" }    ┌──────────────┐
   │ bot a        │ ────────────────────────▶ │ bot b        │
   │ controller   │  b's inbox trigger:       │ controller   │
   │              │  filter/route/coalesce    │              │
   │              │ ◀──────────────────────── │              │
   └──┬───────────┘   bot_emit { to: "a" }    └──┬───────────┘
      │               (slice 2: receipt when    │
   sessions           b's delivery finishes)  sessions
      └─ agent_run / agent_spawn ▶ children      └─ …
```

Horizontal edges are events: durable, asynchronous, never park a session,
bounded by hops and rate. Vertical edges are promises (P134). A session
never parks on another bot, and a child never emits a bot event.

## Design

### 1. One inbox per bot: the `bot` trigger

Nothing reaches a bot without a trigger it declared (webhooks need an
ingest URL, self-emission is a grant). Bot → bot keeps the rule — **the
receiver declares an inbox** — and adds one constraint: **a bot has at most
one trigger of kind `bot`.**

```text
kind: "bot"
spec: { from?: string[] }        // sender bot names; absent = any bot in the universe
filter / route / coalesce / deliver: unchanged — every knob a receiver has
for webhooks it has for bots. The CEL filter sees `event.sender`.
```

Conventionally named `inbox`; the create-bot dialog offers "accept events
from other bots" as a checkbox that creates it. The one-per-bot rule is
validated in the API trigger put and in `bot_trigger_put`.

Why exactly one: `to: "b"` must mean one logical message to B. With several
matching triggers the per-receiver event id would collide with
`bot_events_bot_event_idx` and the extra copies would be silently treated
as duplicates by `onConflictDoNothing` — the worst of both worlds. One
inbox makes the identity unique by construction and removes any need for
trigger "modes"; a receiver that wants different handling per sender or
kind writes it in the filter and the route key, which already see the
whole envelope.

### 2. `bot_emit { to }`, joined for admission

```text
bot_emit { kind, summary, data?, to?: botName, sessionKey? }
```

- `to` absent: **self**, exactly today's path — grant-gated, bypasses
  triggers, `sessionKey` picks a keyed session. No behaviour change for
  existing bots.
- `to` present: **addressed**. `sessionKey` is rejected — a sender never
  chooses another bot's routing; the receiver's inbox owns it. The tool
  activity admits the event through B's inbox with the same pipeline
  webhook ingest uses: enabled bot → enabled `bot` trigger → `from` →
  hop bound → sender rate cap → filter → route → coalesce params →
  `whenBusy` → store-then-wake. That pipeline (`admitBotEvent` and
  the breaker check) moves from `platform/backend` routes into the bots
  package so ingest, poll, self-emit, and addressed emit share one
  function; today the self-emit activity duplicates the insert without it.
- **Joined.** The tool waits for validation and durable storage only —
  never for B — and returns `{ to, seq }` (B's `#N`, the handle bots
  already use; never the digest id, per [P138](p138-model-facing-ids.md))
  or a typed refusal the model reads: `unknown_bot`, `bot_disabled`, `no_inbox`, `not_accepted`
  (`from`), `filtered` (B's filter archived it), `breaker_tripped`,
  `rate_limited`, `loop_cut`. This changes the spec's completion from
  `accepted-push` to `joined` and makes the controller signal the result
  back for `bot_emit` too. It also fixes the current defect that the
  self-emit rate-cap refusal is only an activity row while the model sees
  "accepted".
- Grant: `selfEmit` becomes `emit` — may this bot emit at all, self or
  otherwise. Receivers protect themselves with their inbox.

### 3. Envelope, identity, bounds

- **Document** (`BotEventDocumentV1`): `source: "bot:<sender name>"` for
  every bot-originated event, self included ("self" is a comparison, not
  a string); `sender: { bot }`; `hops`. No session ids in the document —
  the receiver's model sees the sender's name and nothing about its
  internals.
- **One column**: `bot_events.sender_bot_id` (null for world events). It is
  what the rate cap counts and what the "from `a` #12" chip reads.
- **Identity**: the per-receiver event id is
  `bot:<senderBotId>:<digest(invocationId)>`. The tool invocation id is
  stable across activity retries, so a retried emit converges on one event;
  with one inbox per bot the id is unique per receiver.
- **Hops**: `hops` = the causing delivery's highest `hops` + 1 (0 for
  events from the world). The inbox value (`BotEvent`) carries `hops`, so
  the controller can put the active delivery's maximum into the summary
  the tool activity already receives. Admission refuses `hops >
  MAX_BOT_HOPS` (8) with `loop_cut`, recorded on the sender.
- **Rate**: the sender's cap (breaker rate, else 60/hour) counts every emit
  by `sender_bot_id` across the universe, self or addressed. There is no
  fan-out in v1, so this is the whole amplification bound.

### 4. The directory: how a sender knows whom it can address

A catalog in context, not a tool the model must remember and not an enum in
a schema — the P134 answer — published as P136's external catalog so it
never rewrites the prefix of a session that lives for months:

- **One key, `bot:directory`.** Before a delivery, the controller derives
  the directory and puts it as `InputItem::Catalog` on
  `session/context/append`; a same-content put is a no-op. It lists
  **only enabled bots whose inbox accepts this bot** — name and a one-line
  `description` (new column: the brief is the job description *for* a bot,
  the description is what *other* bots see) — or one line saying no bot
  accepts its events. Bots that are not listening do not help the model
  and cost context.
- **A change lands at the tail** as a superseding version; the previous one
  stays rendered byte-for-byte and is the first thing compaction drops. A
  bot learns of a neighbour at its next run, as it learns of events.
- **No run, no budget.** A directory change never wakes the bot.

One declaration — the inbox — drives both routing and discovery, so there
is no sender-side allowlist to keep in sync. The universe is the trust
boundary and the directory carries no authority: the brief says *when* to
use a neighbour, the directory says *who* is there.

### 5. Replies

**v1 has no reply mechanism.** The sender is in every event; if B's brief
says to answer, B answers with `bot_emit { to: event.sender.bot }`, which
lands through A's inbox like anything else. Symmetric, and nothing new.

**Slice 2 adds resolution receipts** — a deterministic answer that does not
depend on B's model remembering to emit one. The whole thing, in order:

1. A calls `bot_emit { to: "b", kind, summary, reply: true }`. Admission
   stores, on B's event row, a private `reply_to`: A's bot id plus the
   emitting session's *logical* route — absent for the main session, or
   `{ sessionId: <base id>, label }` for a keyed one, the same
   `BotEventSession` shape routing uses. Never a concrete generation, never
   in the document. A resolves its own delivery `deferred` ("I asked and am
   waiting"), which gives that label its meaning.
2. B's controller delivers the event as usual; B's model handles it and
   calls `bot_event_resolve { outcome, summary }`.
3. When the delivery finishes, B's controller calls one activity,
   `sendReceipts({ deliveryId, eventIds, status, summary })`. It reads the
   rows that carry `reply_to` and admits into A one event per asked event:
   kind `bot.reply`, `source: "bot:b"`, correlation `{ bot: "b", seq: 17 }`
   — the `#N` the tool result gave A, rendered as "reply to your #17 at
   `b`", never the digest event id (P138) — `summary` = B's summary or
   the status, `data: { status, outcome? }`,
   id `reply:<bBotId>:<deliveryId>:<eventId>` (deterministic; retries
   converge). It is routed to `reply_to.session` — A's controller resolves
   the current generation on delivery as for any routed target — with
   `whenBusy: queue`, and bypasses A's inbox and filter: you always hear
   back on your own asks.
4. A's next run reads "reply to #12 from `b`: …" and resolves `handled`.

`status` is whatever the delivery finished with: B's outcome (`handled`,
`deferred`, `ignored`, `blocked`) with its summary, `run_failed`,
`unresolved`, or — for inboxes that deliver by `append`/`steer` — `appended`
/ `steered`, which is an acknowledgement, not an answer, because those
deliveries finish before any run. A bot that wants to be askable keeps
`queue`, the default. Refusals at admission are the tool result (§2), not
receipts. A receiver that is budget-parked and then disabled never replies;
deadlines are added on evidence, not in advance.

What slice 2 deliberately lacks: sender-side outstanding-ask state, timers,
per-session reply coalescing (a fan-out ask comes back as one delivery per
answer), and any reply B's model authors — the receipt *is* the reply, and
B's `summary` carries the answer.

### 6. Influence without authority

An ops bot works without configuring anyone: it **asks** (`bot_emit { to:
"comms", kind: "tuning.request", summary: "…", reply: true }`); the receiver
**decides** — the event is untrusted data, and `comms`'s human-written brief
says whether to honour tuning requests from `ops`; with `selfConfig` it
applies the change itself and its feed reads "ops asked, comms agreed". The
**hard stops stay deterministic** — flood breaker, `runsPerDay`, poll
auto-disable, hop cut — and re-enabling stays human. Each bot is sovereign
over its own configuration; the human over every bot's scope.

### 7. Rules

1. **Horizontal is events, vertical is promises.** A session never parks on
   another bot. An answer with a bot's *context* is an event and `deferred`;
   a *synchronous* answer is `agent_run` on the bot's profile (no inbox, no
   brief, no bot tools).
2. **Children never emit.** `bot_*` tools are bindings on managed sessions;
   a child has the profile only. Every bot event's sender is a bot session.
3. **No cross-bot authority.** Neighbours ask.
4. **Budgets stay in their tiers**: run budget per bot, tree limits per root,
   hops and emit rate per exchange. No cross-bot budget.
5. **Provenance, never ownership.** No cascades, no lifecycle trees.
6. **Deterministic identities**: event id from (sender, invocation); receipt
   id from (receiver, delivery, event).
7. **A child is never a bot and a bot is never a child** (P134 §8).

### 8. Model-facing ids: `#N` and names, never digests

Everything a bot's model must *echo* is already a counter or a name:
`seq` for `bot_event_read`, nothing for `bot_event_resolve` (the controller
knows the delivery), trigger `name`, `sessionKey` (the label). The two long
ids it can pass — `environmentId` and `grantId` on `bot_trigger_put` — are
rare, belong to other registries, and come from a human's brief; they stay.

What is wrong is what the model is *shown* and can never use, verified
2026-08-26: `eventId` on every row of `bot_event_list`, `bot_filter_test`,
and `bot_event_read` (`whk-<64 hex>` 68 chars, `poll:<uuid>:<32 hex>` 74,
`schedule:<uuid>:<iso>` ~62, `self-<uuid>` 41 — a default 20-row list is
~700 tokens of hex beside the `seq` that is accepted), `session.sessionId`
per row, the uuid `id` beside `name` in `bot_trigger_list` / `put`, and in
`bot_status` the `sessions[].sessionId`, `activeDeliveries[].id` (an event
id, or `batch-<64 hex>` for a coalesced batch) and `profileId`. Digests
invite copying, and a weak model that copies one burns a turn.

The rule: **digest ids live in rows and on the API; the model sees `#N`
and labels.** Concretely:

- `bot_event_list`, `bot_filter_test`, `bot_event_read`: drop `eventId`;
  `session` becomes its label (`"incident-42"`, or absent for main).
- `bot_trigger_list`, `bot_trigger_put`: drop `id`.
- `bot_status`: `sessions` → `{ label, kind }`; `activeDeliveries` →
  `{ events: [12, 13], session }` with labels; drop `profileId`; buffers
  keyed by label.
- `bot_emit` returns `{ to, seq }` (§2); receipts correlate by
  `{ bot, seq }` (§5); the federation event id
  `bot:<senderBotId>:<digest>` (~105 chars) and the receipt id never
  appear in a tool result or a rendering. Cross-bot references read
  "#17 at `b`".
- `renderEventPrompt`'s `correlation:` line stays — it is the sender's
  value — but nothing Lightspeed writes there is a digest.
- The web UI and the platform API keep every id; only the model-facing
  payloads in `platform/bots/src/activities/tools.ts` and the controller
  summary change.

### 9. Identity: an authored id and a display name, like a profile

**The platform's pattern**, verified 2026-08-26: a profile is created with
a user-authored `profileId` ("what routing rules and tooling reference —
cannot be changed later", the create dialog derives it from the display
name until edited) and a mutable `displayName`; a session has a
client-authored id (`bot:v1:triage`) and a mutable `display_name`; a
universe has an immutable `slug` as its URL segment beside its uuid row
key. Readable, authored, immutable ids for everything people and models
refer to; labels are separate and mutable.

**Bots today** have the authored id — `name`, `^[a-z0-9][a-z0-9-]*$`,
unique per universe, immutable — and every runtime identity is correctly
built from it: `botWorkflowId` is `lightspeed.bots.v1/<universe>/<name>`,
sessions are `bot:v1:<name>…`, schedules `…/<name>/schedule/<trigger>`,
`source` and the directory say it, `to` / `from` will say it. What is
inconsistent is that the **API and web UI address bots by the uuid row
key** (`/api/v1/bots/<uuid>`, `/u/<slug>/bots/<uuid>`), and that there is
no display name, so the handle has to do double duty as the label.

**Rule.** A bot has an authored id and a display name, and nothing else
names it:

- **`botId`** — today's `name`, renamed on the wire to say what it is
  (`sessionId`, `profileId`, `botId`). Authored at creation, validated as
  today, unique per universe, immutable. It is what models say (`to:
  "infra"`, `from: ["triage"]`, the directory), what humans type in briefs
  and URLs, and what every Temporal and session identity is derived from
  — unchanged. The create dialog derives it from the display name until
  edited, as the profile dialog does.
- **`displayName`** — new, mutable, nullable (falls back to the id);
  `description` (§4) is the one-liner other bots see. Managed sessions get
  `bot <displayName>` / `bot <displayName> · <label>` and the controller
  applies a change with `session/rename`.
- **The uuid row key stays internal**, exactly like `universes.id` beside
  `slug`: foreign keys (`bot_triggers`, `bot_events`, `sender_bot_id`,
  `reply_to`) use it, nothing outside the database does. API routes
  become universe-scoped and addressed by id —
  `/api/v1/universes/:universeId/bots/:botId/…` — and the web URL is
  `/u/<slug>/bots/<botId>`. Responses carry `botId` and `displayName`;
  the uuid disappears from the wire.
- **Triggers get the same treatment**: `name` is the authored id (already
  what `bot_trigger_put` keys on and what schedule ids use); the API
  addresses `…/triggers/:triggerName`; the uuid stays a row key. The
  webhook ingest URL keeps its opaque `<uuid>/<token>` path — it is a
  capability URL, and opaque is right there.
- **No handle rename**, same as a profile id, a universe slug, or a
  session id. Human-written briefs and other bots' `from` lists mention
  the handle; renaming it would silently break that text anyway. The
  label renames freely.

**Why not a uuid identity with a mutable name** (the first answer): it
buys handle renames at the price of an unreadable identity in every
Temporal and session id and a second spelling of every bot in the API —
and the platform has already decided this question the other way for
profiles, sessions, and universes.

## Worked example: an incident team

- `triage` — Sentry/PagerDuty webhooks (`perKey` on incident id), `emit`.
  On an incident it addresses `investigate` to `infra` with `reply: true`
  and resolves `deferred`.
- `infra` — inbox `{ from: ["triage"] }`, a standing `existing`
  environment, `features.subagents` (`log-reader`, `deploy-diff`). It
  `agent_run`s two children, writes a finding, resolves `handled` with the
  finding as summary. The receipt lands in `triage`'s incident session;
  `triage`'s next run posts it.
- `comms` — inbox `{ from: ["triage", "infra"] }`, filter
  `event.kind.startsWith("incident.")`, 30 s coalescing; `triage` and
  `infra` address it explicitly (no publish in v1). Its brief says tuning
  requests from `ops` are applied when they narrow, never when they widen.

No core change, no bot with authority over another, every arrow an event
through admission or a promise through P134.

## What is deliberately not built

- **Cross-bot authority** — see the later note.
- **`bot_ask` / any joined cross-bot call** — deadlock between singletons,
  lane starvation, unbounded latency.
- **Publish/subscribe** — `to` is required for other bots in v1. If a
  use case wants fan-out, it is a `published: true` flag on the same inbox
  trigger plus a recipients-per-emission cap; not before.
- **Reply deadlines, per-session reply coalescing, sender-side ask state.**
- **`bot.*` system events on the bus** — activity rows and the UI already
  observe the deterministic layer; publishing telemetry back onto the bus
  raises loop and amplification questions before there is a consumer.
- **Causation trace UI** — coalescing makes causation a DAG and delivery
  membership is not persisted; `hops` is the bound, the trace is later.
- **Cross-bot session/event access**, **bot-per-entity spawning** (that is
  `perKey`), **cross-universe federation**, **the A2A adapter** (later, as
  another target behind the same `bot_emit`).

## Slices

1. **Identity and ids** — **done 2026-08-26.** §9: `bots.display_name`
   and `bots.description` columns (folded into the pre-release `0002_bots`
   baseline, platform schema revision 3); the wire carries `botId` (the authored
   `name`), `displayName`, `description`, and no uuid; routes are
   `/api/v1/universes/:id/bots/:botId/…` with triggers by
   `:triggerName` and webhook triggers carrying `ingestPath`; the web
   addresses `/u/<slug>/bots/<botId>`, the create dialog derives the id
   from the display name until edited (as profiles do), settings edit
   the label and description; `BotStartV1.displayName` is required and
   the controller applies a change with a new `renameBotSession`
   activity (`session/rename`) to every managed session, recording
   `renamed`. §8: `platform/bots/src/activities/tool-views.ts` holds
   every model-facing shape as a pure function (`bot_status` by `botId`
   and labels, deliveries as `#N`s, no `profileId`; `bot_event_list` /
   `bot_filter_test` / `bot_event_read` without `eventId` or session
   ids; triggers without row keys; `bot_emit` → `{ seq }`) and
   `test/tool-views.test.ts` asserts no uuid and no ≥32-hex digest over
   webhook, poll, schedule, self, and federation event rows plus the
   event rendering. Pulled forward from §2: `bot_emit` is **joined**
   (`BOT_TOOLS_REVISION` 9), so the rate-cap refusal reaches the model.
   Temporal and session identities did not change; no stack reset.
2. **Bus** — **done 2026-08-26.** Folded into the pre-release `0002_bots`
   baseline (platform schema revision 3): the final column is `bots.emit`;
   `bot_events.sender_bot_id`, `hops`, `reply_to` (private route),
   `in_reply_to` (public correlation). `bot` trigger kind with
   `{ from? }`, at most one per bot (validated in `createTrigger`, so both
   the API and `bot_trigger_put` refuse a second), full filter / route /
   coalesce / deliver knobs, the CEL filter sees `event.sender`; the web
   has an "Inbox" trigger kind and the create dialog's "accept events
   from other bots" creates it (`acceptsBotEvents`). One admission
   pipeline in `platform/bots/src/admission.ts` — `storeBotEvent`
   (store-then-wake) and `admitTriggerEvent` (filter → route → coalesce →
   delivery policy → store) — now used by webhook ingest, polls,
   schedules, self and addressed emits, and receipts. `bot_emit { to,
   reply }` is joined and returns `{ to, seq }` or a typed refusal
   (`unknown_bot`, `bot_disabled`, `no_inbox`, `not_accepted`,
   `filtered`, `breaker_tripped`, `rate_limited`, `loop_cut`; the code is
   in the error payload). Ids are `bot:<senderRowKey>:<digest(invocation)>`;
   `hops` rides on the inbox value and the controller hands the invoking
   session's hop count and logical route to the tool activity
   (`BotControllerSummary.invocation`, never shown to the model); the cut
   is `MAX_BOT_HOPS = 8`; the sender rate cap counts every emit by
   `sender_bot_id`. The controller puts the `bot:directory` catalog
   (P136 external catalog) before every delivery of an emitting bot;
   events carry `source: "bot:<id>"` and `sender`; event rows on the API
   carry `sender` and `inReplyTo` and the UI shows "from `a`" / "reply to
   #17 at `b`" / hop chips. Activity kinds: `emitted`, `loop_cut`,
   `replied`, `reply_failed`. Deviation: poll items that miss the filter
   still advance the cursor without an archived row (a busy feed would
   bury the envelope store), so polls filter before the shared pipeline
   and use only its store step.
3. **Receipts** — **done 2026-08-26.** `reply: true` stores the asking
   bot and its logical route on the receiver's row; the controller calls
   `sendBotReceipts` when a delivery carrying an ask finishes (any finish
   status, including `appended` / `steered` acknowledgements) and one
   `bot.reply` per asked event is admitted into the asker's session
   (`whenBusy: queue`, bypassing its inbox), rendered as "reply to your
   #17 at `b`". Receipt ids are `reply:<answeringRowKey>:<digest(delivery,
   event)>`; a receipt that would cross the hop bound is recorded as
   `loop_cut` instead.
4. On demand: publish/subscribe, wiring view, A2A target, deadlines.

Verification 2026-08-26: `npm run check` green; the bots Temporal
integration suite (14 scenarios, including "publishes the directory
before a delivery and sends receipts when it finishes" with history
replay) green; `test:migrations` (empty install and upgrade from the
baseline through 0004) green. A live two-bot exchange through the model
was not run.

## Tests

- **Unit** (`platform/bots/test`): every `bot_*` result and every event
  rendering, over a fixture with webhook, poll, schedule, self, and
  federation events, matches no `/[0-9a-f]{32}/` and no uuid — the
  §8 guarantee, so a new field cannot quietly reintroduce a digest;
  route tests for universe-scoped `botId` / `triggerName` addressing and
  the uuid's absence from every response; inbox matching (`from` allowlist,
  disabled trigger, disabled bot, missing inbox, one-per-bot validation);
  deterministic ids across a retried invocation; `hops` propagation and the
  cut; sender rate cap across self and addressed emits; typed refusal for
  each admission failure; directory rendering (only accepting bots, the
  empty line) and the no-op put; receipt document and id per finish status;
  `reply_to` route for main and keyed senders.
- **Integration** (`BOTS_TEMPORAL_INTEGRATION=1`, fake activities): an
  emitting bot publishes the directory before the run and sends receipts
  with the delivery's status, summary, and hop count for asked events
  only; controller history replay. Admission itself (inbox resolution,
  refusals, hops, the directory, receipts) is unit-tested as pure
  functions in `test/admission.test.ts`; the full `a`→`b`→`a` exchange
  through real activities and a database has no harness yet and is the
  first thing to dogfood on the dev stack.
- **Platform**: `test:migrations` asserts the columns; `npm run check` and
  `check:identity` green; the bots integration suite keeps its scenarios.
