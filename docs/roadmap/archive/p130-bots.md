# Bots: A Proactive Layer Over Managed Sessions

**Status**

- P130. Started as a preliminary product design (ideas and research, not an
  implementation plan); slices 1-4 are now implemented on the `bots` branch —
  see the implementation log below.
- Written 2026-08-20 from a codebase survey (managed sessions, workflow-tool
  spine, Channels, Foundry, profiles/environments) plus industry research
  (xAI, OpenAI, Anthropic, Google, Dust, Lindy, Gumloop, Inngest, Temporal,
  Svix, Hookdeck, Composio, the MCP spec's trigger story). Sources at the end.
- Builds on P100/P100b/P106 (workflow tools, emissions, self-receiver),
  P103 (managed sessions), P125/P126 (profile-provisioned environments,
  power/idle), and the Channels/Foundry controller pattern.
- Revised same day after contrasting with a parallel proposal draft
  (`later/pNNN-bots-alternative.md`): adopted the durable inbox + wake-signal
  ingestion contract and deterministic delivery identities; recorded what was
  deliberately not adopted at the end of the architecture section.
- Implementation started 2026-08-20. Slice 1 stage 1 done: `platform/bots`
  package (controller workflow with dedupe, budget, terminal reconciliation,
  `bot_event_resolve` self-receiver tool, continue-as-new carry), `bots` /
  `bot_events` / `bot_activity` Platform tables (envelope store authoritative,
  store-then-wake), server CRUD + `POST /api/v1/bots/:id/events` endpoint
  trigger + state/events/activity reads, dev-stack full-profile workers, and
  a Temporal integration suite (`BOTS_TEMPORAL_INTEGRATION=1`) covering
  dedupe, resolution reconciliation, budget parking, and history replay.
  Stage 2 done same day: `bot_triggers` table, schedule triggers reconciled to
  Temporal Schedules (overlap SKIP, 5m catchup, paused when trigger or bot is
  disabled), a `botScheduleFireWorkflowV1` that re-reads the trigger row and
  admits a deterministic envelope (`schedule:<triggerId>:<nominalTime>`)
  through store-then-wake, trigger CRUD routes, and integration coverage for
  upsert idempotency, immediate trigger, pause, and delete. A web UI landed
  the same day: a Bots master-detail surface (`platform/web/src/pages/
  BotsPage.tsx` + `components/bot/`) with the live session embedded, a
  control panel (status, budget, triggers CRUD, event inbox, activity feed,
  send-event), and the BotMark glyph in navigation. Slice 1 was validated
  live the same day (schedule fires → runs → `bot_event_resolve` outcomes on
  the dev stack).
- Event input redesign implemented 2026-08-24 (greenfield rework of what
  sessions actually receive):
  - **Run-scoped resolution**: `bot_event_resolve` is now `{outcome,
    summary}` — the controller correlates a resolve by the (session, run)
    it already tracks, so the model never echoes a delivery id and a typo
    can no longer silently land a delivery as `unresolved`. Delivery ids,
    submission ids, and terminal tokens stay internal.
  - **Per-bot sequence numbers**: every admitted event gets `#N` from a
    race-free counter on `bots.event_seq` (migration `0008_bot_event_seq`,
    with backfill); `#N` is the only model/human handle — renderings,
    `bot_events_read`, filter tests, snapshots (`recentEvents[].seqs`), and
    the web event history all use it.
  - **Envelope/rendering split**: the stored `BotEventDocumentV1` stays the
    complete machine envelope (filters, UI, replay, reads); admission also
    stores a compact model-facing rendering (`bot_events.prompt_ref`)
    produced by a generic shape-based renderer (`platform/bots/src/
    rendering.ts`): drops URL/plumbing keys, collapses identity objects,
    caps strings/arrays/depth under a ~2 KB budget, and marks every cut
    with a pointer to `bot_event_read #N`. Deliveries send one rendered
    item per event — no framing item for single events; batches get one
    header line ("N events … resolve the delivery once"). The standing
    protocol (untrusted content, resolve semantics, pruning) moved into the
    session instructions.
  - **GitHub preset projects instead of forwarding**: identity + summary +
    subject-object projection for the prompt (`promptData`); the stored
    document keeps the full body and headers. Payloads without the
    envelope grammar (push) fall back to the full body through the
    renderer.
  - **`bot_event_read`**: joined tool `{seq, path?, maxBytes?}` returning
    the full archived envelope (data, headers, receivedAt, session), with
    dot-path narrowing, a clamped size cap, and honest over-budget replies
    (pruned preview + largest branches by size). Unknown `#N` fails with
    the valid range.
  - **Strict-mode policy**: `strict: true` only where the schema has no
    optional fields (resolve, status, trigger delete, brief put); tools
    with real optionals (`trigger_put`, `filter_test`, `events_read`,
    `event_read`, `emit`) are non-strict with genuinely optional
    properties instead of null-stuffed `required` lists — server-side
    validation with typed retryable tool errors is the contract on every
    provider. `BOT_TOOLS_REVISION` bumped to 4 (sessions rotate).
  - Replays re-render from the original stored document instead of a stub
    summary.
  - Tool-name alignment (2026-08-24): `bot_events_read` renamed to
    `bot_event_list` (`lightspeed.bots.event.list.v1`) to match the core
    `_list`/`_read` vocabulary (`profile_list`, `agent_list`,
    `environment_list`); new `bot_trigger_list` returns the trigger views
    (specs, filters, routing, ingest URLs), which moved out of
    `bot_status` — status is now purely runtime state. Revision 5.
    Selected schema properties then got `description` annotations (cron/at
    exclusivity, CEL fields, coalescing semantics, secret coupling,
    event-read path/cap, emit sessionKey) — only where the name and type
    cannot carry the rule, so the tool definitions stay lean. Revision 6.
  - Self-configuration grant (2026-08-24): `bots.self_config` (migration
    `0009_bot_self_config`, default **off**) gates the mutating tools —
    `bot_trigger_put`, `bot_trigger_delete`, `bot_brief_put` are not even
    declared to sessions without the grant (a toggle flip changes the
    declaration fingerprint, so the existing rotation path applies it), and
    the tool activity re-checks the fresh row as defense in depth (403 for
    stale pre-toggle sessions). Read-only tools, `bot_event_resolve`, and
    `bot_emit` (provenance-tagged, budget-bounded) stay always-on.
    Settings-dialog switch; `bot_status` reports the grant.
  - Hardening round (2026-08-24): (1) routed-session declaration rotation —
    `ensureRoutedSession` now treats a declaration mismatch like the main
    session does: bump the key's generation, retry once, and re-key the
    active lane, so restarts without carry no longer wedge keyed deliveries
    (integration-tested with replay). (2) CEL save-time validation — the
    shared `celInput`/route-key zod schemas parse expressions with cel-js at
    write time, so broken filters fail at `bot_trigger_put`/the API instead
    of silently archiving events; runtime evaluation still fails closed.
    (3) The flood breaker now guards schedule fires too (trip disables the
    trigger and drops its Temporal Schedule, recorded as `breaker_tripped`).
    (4) `bot_emit` moved behind its own grant `bots.self_emit` (migration
    `0010_bot_self_emit`, default off, settings switch, declaration +
    execution enforcement like `selfConfig`), and granted bots are
    rate-capped (bot breaker rate, else 60/hour) with a 429 tool error, so
    self-event feedback loops break before the daily budget.
  - Decided 2026-08-24: no dedicated operator-chat trigger. Messaging a bot
    directly goes through the sessions page's Direct-input override (below);
    the `chat` transport in the proposal body remains the future Channels
    bridge (slice 5), unrelated to operator messaging.
  - Operator direct input (2026-08-24): the sessions-page composer gate on
    managed sessions became an explicit override — a "Direct input" switch
    (off by default, reset on navigation) with a warning that it bypasses
    the manager's ingress, budget, and delivery policies. The engine always
    admitted such runs; the gate was UI policy only. Not yet done: per-trigger CEL projections (tier 2 of the
    extraction design) — the generic renderer plus preset projection cover
    the current needs.
- Slice 2 implemented 2026-08-20, end to end:
  - Triggers reshaped to per-kind `spec` jsonb + `filter` (CEL) + `route`
    columns (backfilling migration). Boot-time schedule reconcile in the
    platform backend converges Temporal Schedules to the rows.
  - **Webhook ingress**: `POST /api/v1/hooks/bots/:triggerId/:token` outside
    session auth; URL-token + optional generic HMAC-SHA256 verification;
    1 MiB cap; credential headers redacted; raw payload preserved in the
    event document. Known tradeoff: webhook secrets sit plaintext in the
    Platform DB jsonb for now.
  - **Filters**: `cel-js` predicates over {event, data, headers}, evaluated
    at admission; non-matching events archive (envelope stored, activity
    `filtered`/`filter_error`, no delivery) — fail-closed on errors.
  - **Routing**: `bot` | `perKey` (CEL key or preset default; readable
    slug + digest session ids) | `perEvent`, computed at admission and
    carried on the envelope; the controller lazily creates routed managed
    sessions (same self-receiver tools), tracks per-session cursors, and
    reports a sessions list in its snapshot. Delivery stays strictly serial
    across sessions for now; keyed-session idle retention/closing is open.
  - **GitHub preset**: `X-Hub-Signature-256` verification defaults,
    `X-GitHub-Delivery` dedupe, `event.action` naming, PR/issue/repo route
    keys.
  - UI: kind-aware trigger management (webhook forms with verification,
    routing, filter; ingest-URL copy), sessions list on the bot page.
  - Tests: webhook helper unit suite (verification, extraction, filters,
    routing), routed-delivery Temporal integration scenario with history
    replay, contracts coverage for routed session ids.
- Slice 3 implemented 2026-08-20, end to end:
  - **Coalescing**: per-trigger `{debounceMs, maxWaitMs, maxCount}` config;
    admission stamps events with a buffer key of (trigger, routed session);
    the controller accumulates per-key buffers, wakes at the earliest flush
    deadline, and flushes the **entire batch as one delivery/run** —
    debounce reset per event, bounded by maxWait, immediate on maxCount.
  - **Per-delivery identity**: `botDeliveryId(sortedEventIds)` (single event
    keeps its id, so pre-batch behavior and replay compatibility hold);
    submission id, terminal token, and `bot_event_resolve` all resolve the
    delivery, not each event. Batch runs carry all event documents plus a
    framing that asks for exactly one resolution with the delivery id.
  - **Busy-session delivery policies** per trigger: `queue` (default,
    wait-for-idle), `steer` (fold the batch into the active run via
    `session/runs/steer`, falling back to a run if it just finished), and
    `append` (keyed `session/context/append`, never starts a run; keys
    `bot:event:<id>` make retries idempotent). Steered/appended deliveries
    do not consume the run budget.
  - **Flood breaker**: per-bot `{fires, windowMs}`; webhook admission counts
    the trigger's recent envelopes, disables the trigger on breach (429,
    `breaker_tripped` activity), and a human re-enables it.
  - **Replay**: `POST bots/:id/events/replay` re-admits a stored envelope
    (same document ref, same recorded routing) as a fresh non-coalesced
    event; envelopes now record `triggerId` and routed `session`.
  - UI: coalescing + busy-policy fields on webhook triggers, breaker in bot
    settings, live buffer display with flush countdown, batch size badges,
    and per-event replay buttons.
  - Tests: coalesced-batch and steer/append Temporal integration scenarios,
    delivery-identity contract coverage; six integration scenarios total.
- Hardening slice implemented 2026-08-23:
  - **Per-session delivery lanes**: the controller dispatches one delivery
    per target session concurrently (main + each routed session); a stalled
    run or lost terminal blocks only its own session. Budget reservations
    count in-flight lanes; emissions resolve to the lane by terminal token.
  - **Routed-session retention**: per-bot `routedSessionCloseAfterMs`; a sweep
    closes idle routed sessions via `session/close` (non-force, busy
    sessions retried later), records `session_closed`, and bumps a
    per-key generation so the next event for that key opens
    `<id>-g2`, `-g3`, … instead of reviving a closed session.
  - **Secret redaction**: trigger reads hide the ingest token and HMAC
    secret from members who cannot manage the bot. Sealing at rest was
    deliberately deferred: the core's grants are write-only by design
    (no secret reveal), webhook secrets are low-value bearer material
    behind filters/breaker/budget, and a platform-side envelope cipher is
    the cheap fix when multi-tenancy makes it matter.
  - Migration `0007_bot_session_retention`; platform schema revision 8;
    `test:migrations` now asserts the bots tables on fresh and upgrade
    paths. Controller changes alter command sequencing, so running dev
    controllers must be terminated once (`temporal workflow terminate
    --query "WorkflowType='botControllerWorkflowV1'"`); they recreate on
    the next event or config change.
- Slice 4 (self-configuration) implemented 2026-08-23:
  - **`bot_*` workflow tools** bound to the controller as pushed **joined**
    tools (60s reply deadline) over the existing self-receiver topology:
    `bot_status`, `bot_trigger_put` (flat model-facing args → the same
    validated create/update path the API uses; webhooks return their ingest
    URL), `bot_trigger_delete`, `bot_filter_test` (CEL against recent stored
    envelopes), `bot_events_read`, `bot_brief_put` (rewrites the brief and
    signals the controller), and `bot_emit` (accepted; self-addressed events
    tagged `bot:self`, optionally keyed). Deliberately not tools: enable,
    budget, breaker, retention, profile, credentials — scope widening stays
    human-only.
  - **Controller answers invocations in their own lanes** and replies by
    signalling the core session workflow (`<universeId>/<sessionId>`) with a
    `source_resolution` whose payload is a CAS ref; invocations dedupe by
    id; failures resolve `failed` with an error ref and record
    `tool_failed`; self-configuration lands in the activity feed as
    `self_configured`.
  - **Session rotation**: tool declarations are immutable per session, so
    the controller stamps a declaration revision and, on a typed
    declaration-mismatch failure from `session/managed/start`, rotates the
    main session to `<id>-g2`, `-g3`, … (`session_rotated` activity) —
    the planned workaround made real.
  - **Shared config module** (`@lightspeed-ai/bots/config`): trigger
    validation + create/update/delete + schedule reconcile, used by both the
    API routes and the bot's tools — one code path. Webhook helpers moved
    to `@lightspeed-ai/bots/webhooks`.
  - **One-shot schedules**: `spec.at` (ISO instant) as an alternative to
    cron, realized as a Temporal calendar spec with explicit year; the
    trigger disables itself and drops its schedule after firing. UI offers
    "Once, at a specific time".
  - Tests: tool round trip via a fake session workflow (reply observed,
    redelivery ignored), rotation on mismatch, declaration shape, arg
    mapping, schedule spec validation; nine Temporal integration scenarios.
- 2026-08-27: a bot's environment is the profile's `existing` environment,
  shown on the bot page with its idle policy; exec pollers default to it;
  bots gained a terminal `close` and a `delete` that closes first — see
  [P140](p140-bot-environments-and-bot-lifecycle.md). The controller's
  `EXTRA_SESSION_CAP` eviction now closes the evicted session instead of
  forgetting it.
- Next: the trigger long tail — poll primitive, agent-authored pollers,
  email, more presets, Channels bridge — extracted 2026-08-24 into its own
  doc, [P131](p131-bot-trigger-long-tail.md). Still open here: secret
  sealing (deferred by decision; now
  [P133](p133-retrievable-grant-leases.md)) and tier-2 per-trigger CEL projections.
  Two defects from the 2026-08-25 external review: **admission was not
  durable** — `admitBotEvent` inserted the row and then signalled the
  controller; if the signal failed, the sender's retry hit the duplicate
  path and returned without signalling, stranding the event. FIXED
  2026-08-25 with one shared `wakeBotController` (`@lightspeed-ai/bots/events`)
  used by the server route, schedule fires, polls, and `bot_emit`: a
  duplicate admission wakes the controller again (it dedupes by event id),
  and a wake that fails right after this call stored the row deletes the
  row before rethrowing, so the retry admits from scratch. Deliberately no
  outbox column or boot scan — a crash between insert and wake still heals
  on the next redelivery of the same event id; add an `admitted_at`
  reconcile only if crash-stranded rows show up or a V2 controller needs
  to drain V1. Still open: **`bot_trigger_put` accepts a raw `secret` from
  the model**, which then transits tool-argument CAS, Temporal history, and
  the activity feed (P133 removes the field).
  Schedule-flood breaker coverage, CEL save-time validation, `bot:self`
  loop capping, and routed-session declaration rotation were closed by the
  2026-08-24 hardening round above.

## The proposal in one screen

Lightspeed today is a passive system: it reacts to inputs. A **Bot** makes it
proactive. A Bot is a universe-scoped record — a brief, a profile, a set of
triggers, a routing/coalescing policy, and budgets — realized at runtime as
**one long-lived controller workflow that owns managed sessions**. The
controller is deterministic code: it ingests events from the world, filters
them, groups them, batches them, and turns them into runs. The intelligence
lives in the sessions it manages, which have full access to everything
Lightspeed already offers — sub-agents, environments, VFS, MCP tools — plus a
small set of `bot_*` workflow tools through which a bot can inspect and
reconfigure itself.

Three findings shape the design:

- **The substrate already exists.** Channels (production) and Foundry
  (frozen) are both already this exact shape: a controller workflow using
  `session/managed/start`, the P100 workflow-tool spine in the proven
  self-receiver topology, and profiles for session construction. Foundry even
  has a generic `POST /events` ingress with dedupe. Bots is a
  *generalization and promotion* of that shape — the decided product future
  for Foundry, and eventually the umbrella over Channels. Only two
  capabilities are genuinely missing from the whole repo: **scheduling** and
  **webhook/email ingestion**.
- **The industry converged on the same definition** — bot = prompt + resource
  grants + triggers, with a trigger trinity of cron, webhook, and API
  endpoint, executed session-per-event on durable workflow infrastructure
  (Dust literally runs Temporal). Lightspeed lands on this frontier natively.
- **Two things nobody ships**: principled event coalescing (many events → one
  run carrying all payloads; Inngest's own feature request for it is stale)
  and agent-authored trigger integrations with a real trust story (Gumloop is
  the lone precedent). Lightspeed has unfair advantages for both — Channels
  already implements the coalescing mechanic, and provisioned environments
  are a ready-made sandbox for bot-written pollers.

The hardcode-vs-dynamic integration question dissolves under a layered
answer: **hardcode transports, not services**. A handful of transport
primitives covers the world; named integrations are thin data-shaped presets
on top; and the long tail is agent-authored pollers running as guardrailed
jobs in the bot's own environment.

## What the field converged on (mid-2026)

| Product | Definition shape | Triggers | Execution | Notable |
| --- | --- | --- | --- | --- |
| xAI Grok Bot | No settings panel; configured by conversation + demonstration | Chat assignment, @mentions, routines panel; no event catalog | Persistent dedicated cloud computer per bot, drives app UIs | The outlier: the *bot* is durable, not the runs; learns its own approval boundary |
| OpenAI ChatGPT Work / Workspace Agents | Named shared agents: instructions + skills + connector scopes | Schedule, event-triggered, monitoring; ~1 run/hour cap | Background runs; visual Agent Builder being shut down | Admin pre-authorization; per-connector read/write scoping |
| Anthropic Routines / Cowork / Claude Tag | Saved prompt + repo/connector access + trigger | Cron, per-routine API endpoint, GitHub webhook (one session per matching PR) | Session per fire; daily caps by plan | Claude Tag: ambient mode + standing instructions in Slack channels |
| Dust | Agent + triggers (NL→cron, webhook with filter language) | Schedule, webhook (GitHub/Jira/generic), @mention | **Temporal**; one conversation per event; 42 runs/day default cap | AI-generated filter expressions; rate limits instead of batching |
| Lindy / Zapier Agents | NL behavior descriptions + trigger/action catalogs | Event catalogs (email, forms, Slack, 9k Zap apps) | Task per event; flood-hold queues | Zapier MCP: 30k actions exposed to any agent |
| Gumloop | Flows + agents | Schedules, webhooks, **AI-written polling triggers** | Run per fire | Agent writes, sandbox-tests, baselines its own poller; read-only, circuit-broken, metered |

Cross-cutting: oversight converges on "act autonomously on read/personal
scope, pause for approval on consequential writes." Flood control is
everywhere filters + daily caps + circuit breakers — *no one* coalesces
related events into a single agent invocation. And MCP settles the tool half
of integrations but has **no trigger story**: the 2026-07-28 spec rewrite
went aggressively stateless (sampling deprecated in favor of pull-based
multi-round-trip requests; tasks became a poll-based extension), and the
official Triggers & Events working group is still in ideation. Triggers need
webhook-native infrastructure regardless of MCP.

## What Lightspeed already has

The codebase survey found the Bots pattern implemented end-to-end, twice:

- **Channels** (production): one indefinitely-lived workflow per conversation
  key; `signalWithStart` ingestion; bounded inbox → dedupe → activation
  classification → **debounce/maxWait/maxMessages batching** →
  `session/managed/start` with itself as lifecycle controller → runs with
  terminal notification → self-receiver tool replies → quiescence-gated
  `continueAsNew`. This is the reference implementation of everything a bot
  controller must do.
- **Foundry** (frozen import): pack CRUD in Platform Postgres, a generic
  `POST /api/v1/foundry/packs/:id/events` ingress (envelope → CAS blob →
  `signalWithStart`, dedupe on event id), a state query for observability,
  profile-driven manager sessions, and run prompts that frame events as
  untrusted input. Architecturally it *is* a proto-bot; P124 froze it "until
  its product future is decided." Bots is that decision.

The managed-session surface is complete for this purpose: idempotent
creation, runs/steer/cancel, `session/context/append` for run-less injection,
config and profile application, environment activation — all proven from
TypeScript activities. Profiles already carry environment intent
(`provision` per session, credentials, retention, idle policy), so a bot is a
session factory for free.

What is genuinely absent:

- **any scheduler** — zero Temporal Schedules usage; the only timers are
  intra-run sleeps and the reaper's tokio loop;
- **any webhook/email ingress** — no signature verification anywhere;
- a push notification transport (the `AgentNotification` enum is dead code;
  everything is long-poll);
- in-place workflow-tool schema evolution on a long-lived session — P100
  deferred that exact problem "until a controller with an indefinite session
  lifetime appears." Bots is that controller; see the frictions section.

**Boundary confirmed:** nothing in this proposal touches the deterministic
core. The engine, the session log, and the API contract stay as they are;
Bots is a client of the existing 91 methods plus two new ingestion concerns
(schedules, webhooks) that live entirely in the platform tier. The one core
change worth considering is the schema-evolution friction below — and even
that has a platform-side workaround.

## The Bot: product surface

A bot is a durable record, edited in the web UI, by the operator CLI, or by
the bot itself through tools. Sketch of the document:

```text
bot: {
  name: "release-shepherd",
  brief: blobRef,                    // standing instructions; the bot's job description
  profile: { kind: "named", profileId } | { kind: "inline", ... },
  triggers: [
    { id, kind: "schedule" | "webhook" | "endpoint" | "poll" | "chat" | "email",
      spec: {...},                   // per-kind: cron+overlap | verification+preset | token | cursor | binding
      filter?: celExpr,              // fires only on matching events
      route: { policy: "bot" | "perKey" | "perEvent", key?: celExpr },
      coalesce?: { debounceMs, maxWaitMs, maxCount },
      deliver: { whenBusy: "steer" | "append" | "queue" } },
  ],
  budgets: { runsPerDay, tokensPerDay?, perTriggerCircuitBreaker: { fires, windowMs } },
  oversight: { approvalScopes: [...], pauseOnBudgetExhaustion: true },
  enabled: true
}
```

At runtime: one **bot controller workflow** per bot (signalWithStart-
addressed, like a Channels conversation or a Foundry pack), receiving
normalized event envelopes and driving sessions. The controller is where
routing, coalescing, budget enforcement, and run lifecycle live —
deterministic, replayable, observable via a state query. The record is the
authority; the controller re-reads it on a config signal.

```text
 Schedule ─┐
 Webhook ──┤   ┌─────────────┐    ┌──────────────────────────┐    ┌──────────────────┐
 Endpoint ─┼──▶│   Ingest    │───▶│  Bot controller workflow │───▶│  Main session    │
 Chat ─────┤   │ verify      │ s  │  1. filter (CEL)         │ r  ├──────────────────┤
 Poller ───┘   │ normalize   │ i  │  2. route key            │ u  │  Session · key A │
 (in env)      │ dedupe      │ g  │  3. coalesce  ◀━ the     │ n  ├──────────────────┤
               │ payload→CAS │ n  │     unshipped piece      │ s  │  Session · key B │
               └─────────────┘ al │  4. deliver + budgets    │    └────────┬─────────┘
                                  └──────────────▲───────────┘             │
                                                 └──────────────────────────
                                            bot_* tool calls · emissions (self-receiver)
```

An event's path: any of the transport primitives lands in a shared ingest,
becomes a normalized envelope (payload in CAS), and is signalled to the
bot's controller, which filters, routes by key, coalesces, and delivers
batches into managed sessions. Sessions talk back over the existing
workflow-tool spine.

## Triggers: three layers, not a catalog

The instinct to dread "writing all these integrations" is correct, and the
resolution is that integrations are not one kind of thing. Everything in the
world reaches a server through one of a handful of transports: something
calls you (webhook/API), something arrives on a protocol (email, chat), you
call something on a cadence (poll), or time passes (schedule). Services
differ only in envelope details — verification scheme, event naming, dedupe
id, payload shape.

### L0 — Transport primitives (hardcoded, finite, ~6)

- **`schedule`** — Temporal Schedules (already a dependency via
  `@temporalio/client`, entirely unused so far), signalling the controller on
  fire. Overlap policies, backfill, pause-with-notes, catchup windows come
  free and are exactly the semantics a bot schedule needs.
- **`endpoint`** — an authenticated `POST /api/v1/bots/:id/events`,
  generalizing Foundry's existing events route. This is the universal escape
  hatch: anything that can make an HTTP call (CI, alertmanagers, Zapier, a
  cron on someone's box) can feed a bot. Anthropic's per-routine API endpoint
  validates this as a first-class product feature, not a fallback.
- **`webhook`** — a per-trigger ingest URL with a verification scheme
  (generic HMAC / basic / API-key / token-in-URL, à la Svix Ingest and
  Hatchet), raw headers+body preserved to CAS, ack-fast-then-process
  (Temporal is the queue, as Channels already proves with `signalWithStart`).
- **`poll`**, **`chat`** (Channels as an event source), and **`email`** —
  the unshipped transports; scoped in
  [P131](p131-bot-trigger-long-tail.md).

One note on the intake plumbing itself: the verification zoo, retry/DLQ, and
replay archive are exactly what Svix Ingest and Hookdeck sell for
$0–500/month. Buying that gateway is a legitimate shortcut — it is cleanly
separable plumbing that delivers into the `endpoint` primitive and is easy to
replace later.

### L1 — Named presets (data, not code)

A preset is a small record over an L0 webhook: which verification scheme and
secret header, where the event name lives (`X-GitHub-Event` + `action`),
where the dedupe id lives (`X-GitHub-Delivery`), sensible default route keys
(PR number, Slack thread, Stripe object id). Svix ships exactly this as a
per-provider scheme table; Hookdeck maintains 180+ of them. GitHub shipped
with slice 2; the further catalog (Slack, Linear, Stripe, Sentry/PagerDuty,
the GitHub App credential story) moved to
[P131](p131-bot-trigger-long-tail.md).

### L2 — Agent-authored triggers (the self-writing layer)

The bot writes its own guardrailed poller for sources with no webhook. The
design (Gumloop guardrail set, daemon jobs in the bot's provisioned
environment, approval-gated activation) moved to
[P131](p131-bot-trigger-long-tail.md).

### The integration option space

| Strategy | Coverage | Cost | Verdict |
| --- | --- | --- | --- |
| Hardcode every service | Whatever you write | Unbounded engineering; Zapier needed a partner ecosystem to reach 9k | Non-starter for a small team |
| Generic transports only | Everything, awkwardly (users wire their own webhooks) | Minimal | Right foundation, insufficient product |
| Buy the catalog (Composio / Pipedream / Paragon) | 800–3,000 apps incl. triggers, managed OAuth | $0.003/trigger-event (Composio); vendor coupling; no ordering guarantees | Viable long-tail accelerator *behind* the endpoint primitive; defer the decision — it plugs into L0 cleanly whenever wanted |
| MCP as trigger surface | — | — | Not possible today: no out-of-band push in the spec; official WG in ideation. MCP stays the *tool* surface (P110) |
| **Layered L0/L1/L2 (recommended)** | Transports cover everything now; presets polish the head; agent-authored pollers absorb the tail | ~6 primitives + preset records + guardrails Lightspeed mostly has | Hardcode transports, not services; let bots extend themselves inside guardrails |

## The router: filter, key, coalesce, deliver

The router is the actual heart of the bot — deterministic code in the
controller, never an LLM. Four stages per event:

**Normalize.** One envelope for everything:
`{eventId, botId, triggerId, kind, source, occurredAt, payloadRef}` —
Foundry's event document generalized, payload in CAS so signals stay small.
Dedupe on `eventId` (provider delivery id where a preset knows one, hash
otherwise); every vendor surveyed delivers at-least-once and unordered, so
idempotent admission is the ground truth, which suits an event-sourced core
perfectly.

**Store, then wake.** The envelope store in Platform Postgres is authoritative
from day one — the activity feed, retention, and replay want it anyway — and
the Temporal signal is a *notification*, never the system of record: ingress
commits the envelope, then signals the controller. At v1 volumes one signal
per envelope is fine (the Channels/Foundry pattern). For high-volume sources
(monitoring bursts, telemetry) the same contract upgrades to a **durable
inbox + wake signal**: ingress advances a per-trigger cursor and sends a
small, coalescible wake (`{triggerId, cursor}` — a doorbell, in the hardware
sense); the controller answers by reading a bounded page of pending envelopes
through an activity and records only its decisions and compact window state
in workflow history. History stays proportional to decisions, not to event
volume, and the upgrade is a delivery detail rather than a data migration
because the store was authoritative all along.

**Filter.** A CEL expression over the envelope + payload (Inngest and Hatchet
both chose CEL; Dust generates filter expressions from natural language —
ours can too, via a `bot_filter_test` tool that replays stored events against
a candidate). Filtering is the first flood defense and the cheapest.

**Route.** A key expression maps the event to a session, under one of three
policies:

| Policy | Session | Use | Precedent |
| --- | --- | --- | --- |
| `bot` | The bot's singleton main session | Digest work, standing supervision, self-configuration | xAI's persistent bot; Claude Tag's channel identity |
| `perKey` (the workhorse) | One session per route key (PR #, email thread, alert incident), created on first event, closed by retention | Ordering and accumulated context per entity, isolation across entities | Claude Code routines' session-per-PR; Temporal entity workflows; Cloudflare DO-per-agent |
| `perEvent` | Fresh session per delivery | Stateless jobs, maximum isolation/parallelism | Dust's one-conversation-per-event default |

The synthesis the research kept circling: *long-lived state, short-lived
reasoning*. Events queue durably in the controller and materialize at
run/turn boundaries — which is precisely Lightspeed's existing admission
semantics (steering lands at turn boundaries; a second run queues). The
engine already thinks this way; the router just extends it upstream.

**Coalesce.** Per (trigger, route key), a buffer with three knobs:
`debounceMs` (quiet period, reset per event), `maxWaitMs` (bound on total
delay from first event), `maxCount` (flush on size) — flushing **the entire
accumulated batch** into one delivery. Each flushed delivery gets a
**deterministic identity** derived from (trigger, route key, sorted event
ids), used as the run submission id — so provider redeliveries, workflow
retries, and worker restarts all converge on the same durable result instead
of duplicate runs. Forty emails become one run that sees
forty envelopes. This exact primitive — debounce timing with a batch
payload — is shipped by no one: Inngest's debounce keeps only the last event
and its issue requesting the combination is stale; Trigger.dev's keeps first
or last. Channels already implements the mechanic (`first + maxWaitMs`,
`last + debounceMs`, maxMessages) for chat; this promotes it to a
first-class, per-trigger policy. It is the flood answer that daily caps
merely approximate, and a genuine differentiator.

**Deliver.** If the target session is idle: `session/runs/start` with a
prompt that frames the batch (envelopes as untrusted input — Foundry's
framing, kept). If a run is active, per-trigger choice: `steer` (fold into
the live run at its next turn), `append` (context injection, no new run), or
`queue` (fold into the next batch) — LangGraph's double-texting taxonomy
(reject/enqueue/interrupt/rollback) is the published vocabulary for this
decision, and Lightspeed's turn-boundary admission model implements the sane
subset natively. Budget enforcement sits here: per-bot runs/day, per-trigger
circuit breaker, pause-vs-disable exactly as Hookdeck distinguishes them
(pause queues, disable drops).

One cheap novelty worth shipping early because nobody documents it: **loop
prevention by provenance**. Tag every envelope whose cause is the bot's own
action — a `bot_emit`, mail sent from the bot's own address, a webhook fired
by a change the bot's session just made — and let filters and circuit
breakers see the tag. Email agents and webhook automations melt down on
exactly this, and no surveyed product has an answer.

A later, optional stage — *triage*, a cheap bounded model call deciding
wake-vs-archive for ambient sources (Claude Tag's trick) — should stay out of
v1. Deterministic filters first; spend model tokens on the work, not the
routing. When triage does arrive, constrain it hard: no action tools, a
bounded structured batch in, a typed decision out
(`ignore | accumulate | activate`), a cheap separate model/profile, and a
visible reason code in the activity feed. The main session must never be
woken just to decide whether it should have been woken.

## Self-configuration and oversight

The bot's main session gets `bot_*` tools over the proven self-receiver
topology (controller = lifecycle controller = tool receiver, the Channels
pattern):

- `bot_status` — triggers, buffers, budgets, recent deliveries (the
  controller's state query, surfaced as a tool).
- `bot_trigger_put / delete` — create and tune schedules, filters,
  coalescing, route keys.
- `bot_filter_test` — replay retained envelopes against a candidate filter
  before committing.
- `bot_poller_propose` — submit L2 poller code + requested credentials for
  activation.
- `bot_emit` — post a synthetic event (bots composing bots; also the loop for
  "remind me in 3 weeks": the bot schedules itself). A self-created reminder
  materializes as a visible, pausable schedule trigger on the record — never
  as a hidden in-run sleep — so it is inspectable and outlives the session
  turn that created it.

So "configure by conversation" — the xAI surface everyone will expect — comes
for free: tell the bot what to watch and it sets up its own triggers. The
record stays the single authority, every change lands in the activity feed,
and the UI edits the same document the tools do.

The trust line follows the industry consensus and GitHub's mechanical
precedent (permissions gate what you may subscribe to): **within granted
scopes, bots self-configure freely; anything that widens scope goes through a
human** — new credentials, first activation of authored code, write-capable
tools on a new surface. Everything a trigger ingests is data, never
instructions: run prompts frame envelopes as untrusted input, which Foundry
already does.

**Observability** is most of the product at enterprise sales time: a per-bot
activity feed showing event → decision (filtered / coalesced-into /
delivered / breaker-tripped) → run → outcome, backed by envelope retention
with replay (Svix/Hookdeck table stakes; also the debugging story), plus the
controller state query for live status. All of it is the Foundry CRUD +
state-query template, generalized.

## Architecture and known frictions

**Where it lives:** `platform/bots/` — a TypeScript Temporal application
exactly like Channels: bot records in Platform Postgres (the `foundryPacks`
shape), ingest routes in `platform/backend`, one controller workflow type,
activities over `@lightspeed-ai/sdk`. Alternatives considered and set
aside: a core Rust crate (wrong tier — this is product logic that will
iterate weekly, and the core's job is to stay still) and new core API methods
for bot CRUD (unnecessary — Channels and Foundry both live entirely on the
existing 91 methods). The genuinely new engineering is a shared library —
*extract the controller pattern* (ensureManagedSession, emission dispatch
loop, batching, run lifecycle, continueAsNew quiescence) from
Channels/Foundry into something like `platform/controller-kit`, so the third
implementation of this shape is the last.

**Tier decision, revisited 2026-08-24.** Moving bots into Rust — into the
server or as a standalone crate — was weighed again, this time for secrets
access, and rejected. Simply put: the leak was a contract, not a tier. The
controller is ~1.2k lines of concurrent workflow code on a pre-1.0 Rust
Temporal SDK; the bots cadence (eight tool revisions in four days) does not
fit the core release train; and bots depend on platform-only RBAC, ingress,
and UI. What the move would have bought is covered separately: the
hand-mirrored emission contract by [P132](p132-workflow-contract-export.md),
service access to secrets by [P133](p133-retrievable-grant-leases.md).
Revisit only if bots become the sole session nucleus (option D of the
fleet-vs-bots review), the Rust SDK reaches 1.0, or API-shaped answers keep
piling up. One further named trigger: once P131's trigger long tail has
landed and the bot record shape has held still, bot records may become
core-owned universe resources with the platform as membership proxy (the
profile / MCP-server / environment pattern). Any Rust controller then
arrives as a `BotControllerV2` with V1 controllers drained and migrated —
Rust cannot replay TypeScript histories.

**Frictions to plan around**, all flagged in the codebase survey:

- **Immutable workflow-tool bindings vs. indefinite sessions.** A bot's main
  session may live for months, but tool declarations are frozen at
  creation — P100 deferred exactly this case. Platform-side workaround for
  v1: *session rotation* — the controller opens a successor session (new
  bindings), carries a compacted summary via `context/append`, retires the
  old one. Workable, but if bots become the center of the product, in-place
  declaration evolution (an add-only admission, like system bindings) is the
  one core change worth its cost. Decide after v1 contact.
- **Managed sessions can't clone/fork.** Fine for controllers, but it means a
  bot's session history can't be branched for debugging; note it, live with
  it.
- **No push transport.** The activity feed and bot UI ride Platform Postgres
  + long-poll like everything else; don't resurrect the dead
  `AgentNotification` enum casually.
- **Always-on economics.** Schedules and pollers burn money while nobody
  watches. Budgets are not optional polish; they're slice-level requirements
  (Gumloop's ~288 credits/day per 5-minute poller is the cautionary tale;
  Anthropic's daily run caps are the crude fix; per-bot budgets + coalescing
  is the better one).

**Deliberately not adopted** (contrasted with the parallel proposal draft,
which arrives at the same architecture — platform-tier controller workflow,
transport-not-service integrations, CEL filters + windows, the same session
topology and admission modes — but specs it several levels deeper):

- **A connector SDK + manifest + registry + certification pipeline.** That is
  the Zapier Developer Platform — a marketplace product that only pays off
  with a partner ecosystem. L1 presets-as-data plus the `endpoint` escape
  hatch plus an optional vendor covers the same ground for a small team;
  build an SDK when third parties are actually asking to write connectors.
- **Source generations with blue/green publish** (create N+1 → prove healthy
  → drain N). Premature: put-with-expected-revision — the repo's existing
  registry idiom — plus stamping envelopes with the trigger revision they
  were admitted under gets most of the value.
- **A seven-noun vocabulary** (connector / connection / trigger type / source
  / event / activation / route). Trigger, envelope, and route suffice until
  they demonstrably creak; extra nouns leak into UI, API, and support.
- **Multiple merge strategies and a K8s-style `BotSpec` YAML document.**
  Full-batch delivery is the one merge semantic that matters (digests are a
  prompt-side concern, provider deltas a preset concern), and Lightspeed's
  config idiom is sparse documents put whole, not `kind`/`metadata`/`spec`.
- **Execution adapters for Work/Flows.** Speculative dependencies — P101 was
  never built. A route starts runs; other execution policies can slot in
  later without being foundations now.

## Slicing (sketch, not a plan)

1. **Bot core.** Record + CRUD + controller workflow (generalized Foundry) +
   `endpoint` trigger + `schedule` trigger (first Temporal Schedules usage) +
   singleton main session + activity feed. Immediately dogfoodable: a nightly
   repo-triage bot, an on-demand alert-ingest bot. This alone matches
   Anthropic Routines' surface.
2. **Webhooks + routing.** Per-trigger ingest URLs, generic verification
   schemes, CEL filters, `perKey`/`perEvent` routing, GitHub preset. (A
   PR-shepherd bot becomes possible.)
3. **Coalescing + budgets.** The debounce/maxWait/maxCount buffers with
   full-batch delivery, busy-session delivery policies, circuit breakers,
   envelope retention + replay. (The differentiator ships here.)
4. **Self-configuration.** `bot_*` tools, filter test/replay,
   configure-by-conversation onboarding.
5. **The trigger long tail.** Moved to
   [P131](p131-bot-trigger-long-tail.md): poll primitive, agent-authored
   pollers, email, further presets, Channels bridge.

Each slice is independently shippable and each earlier slice is load-bearing
for real use — the classic reason trigger systems die is shipping the catalog
before the routing.

## Sources

- xAI Grok Bot: <https://x.ai/news/introducing-grok-bot>
- Claude Code Routines: <https://claude.com/blog/introducing-routines-in-claude-code>
- OpenAI Workspace Agents: <https://openai.com/academy/workspace-agents/>
- Dust triggers: <https://dust.tt/blog/introducing-triggers-your-agents-working-while-you-sleep>
  and internals <https://deepwiki.com/dust-tt/dust/3.6-trigger-system>
- Gumloop AI-written triggers: <https://docs.gumloop.com/core-concepts/ai_trigger_creation>
- Inngest batching/debounce: <https://www.inngest.com/docs/guides/batching>,
  <https://www.inngest.com/docs/guides/debounce>, and the unshipped
  combination <https://github.com/inngest/inngest/issues/3695>
- Temporal Schedules: <https://docs.temporal.io/schedule>; ambient agents:
  <https://temporal.io/blog/orchestrating-ambient-agents-with-temporal>
- Svix Ingest: <https://docs.svix.com/ingest/receiving-with-ingest>;
  Hookdeck: <https://hookdeck.com/docs/connections>
- Hatchet webhooks: <https://docs.hatchet.run/v1/webhooks>
- Composio triggers: <https://docs.composio.dev/docs/triggers>
- MCP Triggers & Events WG: <https://modelcontextprotocol.io/community/triggers-events/charter>
- LangGraph double-texting: <https://docs.langchain.com/langgraph-platform/double-texting>
- GitHub App webhooks (permissions gate subscribable events):
  <https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/using-webhooks-with-github-apps>
- Codebase: `platform/channels/`, `platform/foundry/`,
  `docs/roadmap/archive/p103-managed-session-api.md`, P100/P100b/P106 docs,
  `docs/roadmap/p125-profile-provisioned-environments.md`.
