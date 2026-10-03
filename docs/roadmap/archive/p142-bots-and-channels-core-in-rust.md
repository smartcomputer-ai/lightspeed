# P142 — Bots and Channels Core in the Rust Runtime

**Status**

- **Slices 1–4 implemented 2026-08-30**: the Rust core (slices 1–3) and
  the Platform cut-over (slice 4 — passthrough routes, web/demo on the
  generated types, `platform/bots` and the platform's bot/channel schemas
  deleted). Implementation log at the end of this document.
- Proposed 2026-08-30 from a design conversation with Lukas, after a
  survey of `platform/bots`, `platform/channels`, `platform/workers`, the
  Platform backend/db/web glue, and the Rust runtime crates.
- Direction from Lukas: move bots and the core of Channels (workflows,
  control plane, tables) into `temporal-workflow`, `temporal-runtime`,
  `store-pg`, and dedicated domain crates; keep the messaging bridges
  (Telegram, WhatsApp) as TypeScript processes in `platform/`; fold the
  bot and channel API into the existing core API — no second gateway.
  Running the product should mean running one server node (gateway,
  sessions, jobs, bots, channels core), still splittable into the roles
  that exist today plus a workflow/activity split. Greenfield: no
  compatibility layer, no data migration, identifiers may change.
- Explicitly reverses the [P124](p124-first-party-platform-monorepo.md)
  non-goal "folding the TypeScript product plane into Rust crates" for
  bots and Channels core. Users, orgs, auth, the web UI, the operator CLI,
  Configurator MCP, and the connectors stay in TypeScript.
- Builds on P100/P100b/P106 (workflow tools, emissions, receivers), P103
  (managed sessions), P130–P141 (bots, federation, chat triggers, bot
  lifecycle, console), P132 (workflow contract export), P133 (retrievable
  grants), P134 (`SubagentExecutionWorkflow` as the Rust receiver
  precedent), P136 (catalogs), P138 (model-facing ids).

## Why

Bots and Channels are already designed as core concepts — a bot owns
managed sessions through the lifecycle-controller protocol, a chat
conversation is the receiver of `message_*` workflow tools, every event
goes through one admission pipeline — but they run as a second product
plane written against the core's public API:

- The Platform backend is itself a Temporal client (`bot-common.ts`,
  `bot-schedules.ts`): signal-with-start, queries, and Schedule
  reconciliation at boot happen in route handlers.
- Two databases hold one product: `bots`, `bot_triggers`, `bot_events`,
  `channel_*` live in `lightspeed_platform`, with `universe_id` foreign
  keys into the Platform's own `universes` table, while sessions, profiles,
  environments, grants, and the CAS live in `lightspeed`.
- The TypeScript controller re-implements core protocol over HTTP: eleven
  `session/*` methods, blob round-trips for tool arguments, a hand-rolled
  `deliver_emission` receiver, grant leases with a process-local cache.
- Development and deployment carry four extra worker processes
  (`bots-workflows`, `bots-activities`, `channels-workflows`,
  `channels-activities`) with their own task queues, env groups, and
  health/metrics ports.

The Rust runtime already has everything the port needs: a domain-crate
pattern (`environments`, `mcp`, `profiles`), embedded ledgered migrations,
the `api_methods!` manifest, a trusted in-process managed-session entry
point (`start_managed_session_for_workflow_with_profile`), a receiver
workflow template (`SubagentExecutionWorkflow`), universe resolution from
workflow ids, and a Temporal client (`temporalio-client 0.4.0`) with typed
signal-with-start, Schedules (`cron_strings`, overlap policy, catch-up
window), cross-queue activity scheduling (`ActivityOptions::task_queue`),
and `WorkerTaskTypes` for the workflow/activity split.

## The shape

```text
                           lightspeed-runtime (one node; --roles gateway,sessions,bots,channels)
                           ┌──────────────────────────────────────────────────────────┐
  Platform (TS)            │ gateway                                                  │
  ┌──────────────────┐     │   JSON-RPC: session/* profiles/* … bots/* channels/*     │
  │ users · orgs     │────▶│   HTTP:     POST /hooks/bots/{trigger}/{token}           │
  │ web UI · CLI     │     │                                                          │
  │ passthrough only │     │ worker roles (one task queue each)                       │
  └──────────────────┘     │   sessions: AgentSession · Subagent · EnvironmentJob     │
                           │   bots:     BotController · BotTriggerFire               │
  Connector host (TS)      │   channels: ChannelConversation                          │
  ┌──────────────────┐     │   + each role's activities and background loops          │
  │ telegram ×N      │──┐  │                                                          │
  │ whatsapp ×N      │  │  │                                                          │
  │ (N accounts, any │◀─┼──│ store-pg: sessions … bots bot_triggers bot_events        │
  │  universes)      │  │  │           channel_accounts channel_pairings              │
  └──────────────────┘  │  └──────────────────────────────────────────────────────────┘
        │ activities on │        ▲ channels/inbound/admit (JSON-RPC, service scope)
        │ the connector │────────┘
        │ task queue    ▼
   Telegram / WhatsApp
```

- **The core owns bots and channels core.** Records, admission, the
  controller, the conversation workflow, tool execution, Schedules,
  receipts, the `#N` counter, and every table are Rust and `store-pg`.
- **Connectors are bridges, nothing more.** A connector normalizes a
  provider's messages into one envelope and sends it to the core
  (`channels/inbound/admit`); it receives work back as three Temporal
  activities on its own task queue (`deliver_channel_message`,
  `prepare_channel_media`, `maintain_channel_typing`) — the same
  outbound seam as today, unchanged. It knows nothing about bots,
  triggers, sessions, or pairing state.
- **The Platform keeps people.** Users, organizations, memberships, the
  universe ↔ organization mapping, the web UI, and the operator CLI. Its
  bot and channel routes become universe-scoped passthroughs to
  `bots/*` and `channels/*`, exactly like its seventy existing profile /
  session / environment passthroughs. It stops being a Temporal client
  and stops owning any bot or channel table.

## Design

### 1. Crates

Two new domain crates follow the `environments` / `mcp` / `profiles`
pattern — records, validation, errors, pure logic, `#[async_trait]` store
traits, an in-memory store for tests — with no I/O:

**`crates/bots`** (depends on `api`, `harness` for storage ids, `sha2`,
`cel-interpreter`, `cron`, `hmac`)

- Records: `BotRecord`, `BotTriggerRecord` with the five `TriggerSpec`
  kinds (`schedule`, `webhook`, `poll`, `bot`, `chat`), `BotEventRecord`,
  route / coalesce / deliver policies, poll cursor state, event outcome
  vocabulary, refusal codes.
- Validation: bot name, cron (5-field or `@macro`, rejects Quartz), CEL
  parse at save time, coalesce and poll bounds, one inbox per bot, chat
  triggers must route `perKey` / `perEvent`, webhook secrets never in
  specs (grant ids only).
- Pure pipeline pieces ported from `webhooks.ts`, `rendering.ts`,
  `poll.ts`, `admission.ts`, `tool-views.ts`, `contracts/bots.ts`:
  webhook verification (URL token, `hmac-sha256`) and the GitHub preset
  projection; CEL filter evaluation over `{event, data, headers}`
  (fail-closed); `compute_route_session`; event document and prompt
  rendering with the size budget and the `bot_event_read #N` footer; poll
  item extraction, cursor diff, baseline rule; inbox resolution, hop
  bound (`MAX_BOT_HOPS`), receipt documents, directory rendering; every
  identity derivation (session ids, event ids, delivery ids, submission
  ids, terminal tokens, workflow and schedule ids); the ten `bot_*` tool
  declarations (schemas, descriptions, gating by `selfConfig` / `emit`);
  the model-facing views that carry `#N` and labels and never digests.
- Store traits: `BotStore`, `BotTriggerStore`, `BotEventStore`
  (including `allocate_event_seq`, write-once `record_outcomes`, rate
  windows by trigger and by sender, the notify/reply-to reads receipts
  need), plus `InMemoryBotStore`.

**`crates/channels`** (depends on `bots`, `sha2`)

- Records: `ChannelAccountRecord` (universe-scoped, authored id, provider,
  provider account id, credential grant id, settings, enabled),
  `ChannelPairingRecord`.
- Wire shapes shared with connectors: `NormalizedInbound` (+ media refs),
  `ChannelDeliveryCommand` / `ChannelDeliveryResult`,
  `PrepareChannelMedia*`, `MaintainChannelTyping`, the admission
  decision enum.
- Pure logic ported from `policy/*`, `identity/ids.ts`,
  `workflows/state.ts`, `workflows/delivery-plan.ts`,
  `media/validation.ts`, `presentation/text.ts`, `contracts/tools.ts`:
  activation classification, control commands, access policy, conversation
  key / workflow id / delivery task queue / pairing key derivations,
  conversation state and compaction, chunked delivery planning, media
  MIME/size validation, the four `message_*` declarations.
- Store traits: `ChannelAccountStore`, `ChannelPairingStore`, in-memory
  impl.

**Existing crates**

- `api`: `src/bots.rs`, `src/channels.rs` DTOs and the `bots/*`,
  `channels/*` method manifest lines (§3). Domain crates depend on `api`
  for wire DTOs the way `profiles` does; the workflow signal payloads
  (`BotEvent`, `BotDeliveryReceipt`, `ChatInbound`) are domain types, not
  API DTOs.
- `temporal-workflow`: `workflows/bot_controller.rs`,
  `workflows/bot_trigger_fire.rs`, `workflows/channel_conversation.rs`,
  their activity definitions, and contract-export roots and vectors for
  everything a connector must derive (§7).
- `temporal-runtime`: `bots/` module (admission pipeline, schedule
  reconciler, tool executor, session ensure/rotate), `worker/activities/
  {bots,channels}.rs`, `gateway/service/{bots_api,channels_api}.rs`,
  `gateway/hooks.rs`, and the `--task-types` role knob (§6).
- `store-pg`: `migrations/008_bots.sql`, `migrations/009_channels.sql`, `src/bots.rs`, `src/channels.rs`.

### 2. Tables

Two migrations, `008_bots.sql` and `009_channels.sql`, in the core schema (`REQUIRED_SCHEMA_REVISION`
→ 9), following `006_agent_profiles.sql` conventions: composite primary
keys on `(universe_id, …)`, `REFERENCES universes(universe_id) ON DELETE
CASCADE`, `*_ms bigint` timestamps, `document_json jsonb` for the sparse
configuration document, `COMMENT ON` for every table and column.

| Table | Key | Holds |
|---|---|---|
| `bots` | `(universe_id, bot_id)` | authored immutable `bot_id` (the `name` today), `revision`, mutable document (display name, description, profile id, brief, runs per day, breaker, routed-session TTL, `self_config`, `emit`, `enabled`), `event_seq`, `closed_at_ms`, `closed_sessions` |
| `bot_triggers` | `(universe_id, bot_id, trigger_id)` | authored `trigger_id` (the `name` today), `revision`, `kind`, spec document, filter, route, coalesce, deliver, `session_close_after_ms`, poll cursor, `enabled`, `disabled_reason`, filter error |
| `bot_events` | `(universe_id, bot_id, event_id)` + unique `(universe_id, bot_id, seq)` | the envelope row in five groups: what arrived (trigger, kind, summary, occurred/received at, `document_ref`), the delivery plan computed at admission (`prompt_ref`, routed session, media), federation (sender bot, hops, `in_reply_to`), the private `receiver` (the admitting chat workflow with its receipt token and receiver-bound tools, or the asking bot of a `bot_emit { reply }`), and the write-once outcome (`outcome`, detail, `run_id`, resolved at). `source` and `delivery_id` are not columns: the source is the trigger or the sender bot, and deliveries live in the controller's history |
| `channel_accounts` | `(universe_id, account_id)` | authored `account_id`, `provider`, `provider_account_id` (unique per universe and provider), display name, `credential_grant_id`, settings, `enabled` |
| `channel_pairings` | `(universe_id, pairing_key)` | trigger, account, chat id, paired at |

Differences from the Platform schema, all deliberate:

- Row keys are the authored ids (`bot_id`, `trigger_id`, `account_id`),
  never a uuid; the uuid row keys existed only because Drizzle. Every
  Temporal identity and session id already derives from `bot_id`.
- `channel_accounts` is universe-scoped (it was deployment-global with
  no owner). A provider account belongs to the universe whose bots it
  serves; the connector reaches the core with that universe's API key.
- `channel_identities` is deleted. It had no writer and existed only to
  join Platform org membership into chat access control (§7).
- Bot and trigger documents get a `revision` so the API can be
  put-with-expected-revision like profiles and MCP servers.
- Event payloads stay in the CAS (`BlobStore`), now in-process.

### 3. API

Everything folds into `AgentApiService` under the existing universe
scoping (the universe comes from the credential, never a parameter) and
the naming rules the contract tests enforce.

**`bots/*`** (universe scope)

| Method | Semantics |
|---|---|
| `bots/create` | insert the record, signal-with-start the controller; may carry `triggers` (the wizard's one-shot create) with full rollback |
| `bots/put` | replace the mutable document with `expectedRevision`; closed bots accept label-only edits; signals `bot_config` |
| `bots/read`, `bots/list` | record views; `list` carries the roster fields (`triggerCount`, `pendingCount`, `lastEvent`) |
| `bots/close` | terminal: write `closed_at`, disable triggers (`bot_closed`), drop Schedules, signal `bot_config { closed: true }`; returns once signalled — an acceptance boundary like `session/runs/start`; `bots/state/read` shows `closing` → `closed` |
| `bots/delete` | close if needed, wait (bounded) for the controller to complete, `session/delete` the recorded sessions, delete the row |
| `bots/state/read` | the controller's `bot_state` query joined with sub-agent lineage from `session/list` |
| `bots/sessions/rotate` | signal `bot_session_rotate` after validating ownership from the state query |
| `bots/triggers/put` | create when `expectedRevision` is null, otherwise replace; validates grants and channel accounts; reconciles the Temporal Schedule with rollback |
| `bots/triggers/read`, `bots/triggers/list`, `bots/triggers/delete` | secrets redacted for non-managing principals; `delete` drops the Schedule first |
| `bots/events/admit` | manual admission through the shared pipeline |
| `bots/events/replay` | re-admit from the stored document with the original routing |
| `bots/events/list`, `bots/events/read` | keyset-paged log; full envelope by `#N` |
| `bots/filters/test` | evaluate a CEL filter against a payload or recent envelopes (the UI wants what the tool has) |

`POST /bots/reconcile` disappears: `profiles/put` signals `bot_config` to
every open bot on that profile, because the core now knows both sides.

**`channels/*`** (universe scope unless noted)

| Method | Semantics |
|---|---|
| `channels/accounts/create`, `put`, `read`, `list`, `delete` | account records (provider, provider account id, credential grant, settings, enabled), managed by the universe's admins |
| `channels/inbound/admit` | **service scope** (connector principals): control plane + signal-with-start of the conversation workflow; returns the admission decision so the connector can author pairing replies natively |
| `channels/pairings/list`, `channels/pairings/delete` | operator visibility and un-pairing |
| `channels/conversations/read` | the conversation workflow's state query, for debugging |
| `operator/channels/accounts/list` | **operator scope**: every enabled account across universes, each with its universe id and credential grant — the connector host's discovery call |

**HTTP** — `POST /hooks/bots/{triggerId}/{token}` on the gateway, outside
RPC auth, next to `/auth/callback`: constant-time token check first,
1 MiB body cap, header sanitization, HMAC verification with the grant's
secret read through the in-process broker, closed → 410, disabled → 409,
breaker → 429, then the shared admission pipeline. Ingest URLs are built
from `LIGHTSPEED_PUBLIC_BASE_URL` (the Platform may proxy
`/api/v1/hooks/...` for URL stability, but the core is the authority).

Counts: roughly 93 → 116 methods. `export-schema`,
`export-workflow-contract`, and `npm run check` regenerate
`crates/api/contract`, `clients/typescript`, Configurator MCP, and the
web types; the Platform web UI switches its bot types to the generated
ones.

### 4. Workflows

All Rust workflow ids are `{universe}/{second}` so
`WorkerActivities::state_for` resolves the universe by `split_workflow_id`
with no new machinery. Signal and query names drop the `_v1` suffix to
match the Rust side (`submit_admissions`, `deliver_emission`).

**`BotControllerWorkflow`** — id `{universe}/bot-{botId}`. A port of
`bot-controller.ts`, same state machine: signals `bot_event`,
`bot_config`, `bot_session_rotate`, `deliver_emission`; query
`bot_state`; store-then-wake dedupe; coalescing buffers with debounce /
max-wait / max-count; one lane per session plus one steer/append sidecar;
UTC-day budget with descendant counting; declaration-mismatch rotation;
routed-session retention sweep; `bot.reply` and `bot_delivery` receipts;
teardown on `closed`; continue-as-new with the carry. Detached TypeScript
lanes (`void runDelivery(...)`) become boxed futures raced with
`select_all` beside the signal-driven loop, as the session workflow
already does (no `FuturesUnordered` in workflow code). The `bot_state`
handler stays pure. The coalesce-wake patch is not ported — greenfield.

**`BotTriggerFireWorkflow`** — one workflow replaces the two TypeScript
fire workflows; the Schedule action starts it with `{ triggerId, kind }`
at id `{universe}/botfire-{triggerId}` (Temporal appends the nominal
time), and it reads `TemporalScheduledStartTime` then runs one activity:
`admit_schedule_event` or `poll_trigger`, with per-kind timeouts and
retry (poll absorbs `EnvironmentNotReady` wake latency exactly as today).

**Temporal Schedules** stay the mechanism for `schedule` and `poll`
triggers (overlap `Skip`, 5-minute catch-up, paused with the trigger or
bot). Modelling them as controller-internal timers was considered and
rejected: it would put every fire into the controller's history and tie
firing to the controller's park state. This is the repository's first
use of `temporalio_client::schedules`; the schedule reconciler converges
every `schedule` / `poll` trigger at boot (a `UniverseRuntime` pass next
to the environment reconciler) and on every trigger put/delete.

**`ChannelConversationWorkflow`** — id
`{universe}/chat-{provider}-{digest}`, one per (universe, account, chat,
thread). A port of `conversation.ts`: signals `chat_inbound`,
`deliver_emission`, `bot_delivery`; query `chat_state`; inbound dedupe,
control commands, activation classification, media preparation, chat
event emission through admission, `message_*` receiver with the single
`reply` promise and `source_resolution` back to the session workflow,
`#N` handle cache with the `resolve_chat_handle` fallback, typing scope
on `started`, text-reply fallback on `finished`, continue-as-new.
Core-side activities run on the `channels` role's queue; the three connector
activities are scheduled with `ActivityOptions { task_queue:
connector_queue }` — the seam that keeps connectors in TypeScript.

Session ids are unchanged (`bot:v1:{botId}`, `…:k-…`, `…:e-…`).
`session/start` rejects session ids with the reserved prefixes
`bot-`, `botfire-`, `chat-`, `envjob-` so user sessions cannot collide
with system workflow ids.

### 5. Activities and service

Bot activities run in the `bots` role (same process as the session
workflow by default) and call the in-process service — `UniverseState.api` (`GatewayAgentApi`) — not
JSON-RPC over HTTP:

- Session lifecycle: `ensure_bot_session` through
  `start_managed_session_for_workflow_with_profile` plus profile apply
  and declaration validation; `rename`, `read_status`, `read_run_usage`,
  `start_run` (with `submissionId` and `notifyOnTerminal`), `steer_run`,
  `append_context`, `close_session` (descendants first), `count_descendants`,
  `read_workflow_tool_invocations`, `read_json_blob`.
- Admission: `admit_schedule_event`, `poll_trigger` (HTTP source with
  leased credentials through the in-process broker; exec source through
  the environment-jobs service), `emit_chat_event`, `store_chat_sent`,
  `resolve_chat_handle`, `record_event_outcomes`, `record_bot_closed`.
- Federation and receipts: `publish_bot_directory`, `send_bot_receipts`,
  `send_delivery_receipts` (external signal `bot_delivery` to the notify
  endpoint).
- `execute_bot_tool`: the ten `bot_*` tools, gated from the fresh row
  (`self_config`, `emit`) as defense in depth, results and errors as CAS
  blobs, `reply` promise resolved by signalling the holder session.

The admission pipeline (`temporal-runtime/src/bots/admission.rs`) is one
Rust function set used by every path — hook route, manual admit, replay,
schedule and poll fires, chat emit, `bot_emit`, receipts: closed-bot
guard → `allocate_event_seq` → render → CAS put → insert `ON CONFLICT DO
NOTHING` → adopt the stored `seq` / refs on duplicate → signal-with-start
the controller (`WorkflowStartOptions::start_signal`) → delete the row
and rethrow if the wake fails and this call inserted it. Filter misses
store nothing.

Tool execution and the `bots/*` methods share the same service functions
(`temporal-runtime/src/bots/`), so a bot editing its own trigger and an
operator editing it through the API go through identical validation.

### 6. Runtime roles

One binary; roles compose in one process by default and split by flag.
A worker role is a task queue plus the workflow types, activities, and
background loops that belong to that subsystem:

| Role | Task queue (default) | Workflows | Activities and loops |
|---|---|---|---|
| `gateway` | — | — | HTTP/JSON-RPC, hooks route, environment reconciler, power reaper |
| `sessions` | `lightspeed-sessions` | `AgentSessionWorkflow`, `SubagentExecutionWorkflow`, `EnvironmentJobWorkflow` | llm, tools, storage, preprocess, environment jobs, workflow tools, promise reaper (today's worker) |
| `bots` | `lightspeed-bots` | `BotControllerWorkflow`, `BotTriggerFireWorkflow` | bot session/admission/tool/federation activities, schedule reconciler |
| `channels` | `lightspeed-channels` | `ChannelConversationWorkflow` | channel activities |

```
lightspeed-runtime                                   # all roles, one process (default)
lightspeed-runtime --roles gateway
lightspeed-runtime --roles sessions,bots,channels    # today's "worker"
lightspeed-runtime --roles bots --task-types workflows
lightspeed-runtime --roles bots --task-types activities
```

`--roles` (env `LIGHTSPEED_ROLES`) replaces the `gateway` / `worker` /
`both` subcommands; `--task-types workflows | activities | all` (env
`LIGHTSPEED_WORKER_TASK_TYPES`) maps onto `WorkerTaskTypes` and applies
to every worker role in the process — the split we will probably never
use, but it costs nothing to keep. Each enabled worker role is one
Temporal worker on the shared `CoreRuntime` and client. Queue names are
overridable per role (`LIGHTSPEED_TASK_QUEUE_SESSIONS`, `…_BOTS`,
`…_CHANNELS`); the gateway knows all three because it starts sessions,
wakes controllers, and starts conversations, and the defaults are
exported in the workflow contract so connectors and integrators never
guess them.

Per-role queues are what make the split possible: a workflow is routed
by its queue, so `bots` and `channels` each own their workflows *and*
their activities end to end. Cross-subsystem traffic is only ever
signals (`deliver_emission`, `bot_event`, `bot_delivery`, `chat_inbound`)
and workflow starts that carry their queue — never an activity on
another role's queue. Connector delivery queues remain separate by
construction (one per account, served by the connector host). A later
`environments` role (jobs, reconciler, power reaper) would split the
same way; it is not needed now.

Background work: the schedule reconciler belongs to `bots`; the
routed-session TTL sweep stays inside the controller; the gateway role
runs the environment reconciler *and* the power reaper (today the
gateway-only mode skips the reaper — an asymmetry to fix while
rewriting the composition).

`./dev.sh full` becomes runtime + envd + Platform backend + web +
Configurator + the opt-in connector host: the four `platform-workers`
processes disappear. `platform/workers` becomes the connector host.

### 7. The connector seam

The connector host is **one process serving many accounts across many
universes** — one Telegram long-poller or WhatsApp socket per account,
one activity worker per account queue, all in one Node process. Account
ownership is a data question (which universe's grants hold the token,
whose chat triggers a message is matched against); how many pollers a
process runs is an operations question, and the two are independent.
Today it is one process per account; this is fewer. The host has exactly
two dependencies — the core API and Temporal — and reads no database.

1. **Discovery**: `operator/channels/accounts/list` → every enabled
   account for the host's providers, each with its universe id,
   provider account id, credential grant id, and settings; optionally
   narrowed by `LIGHTSPEED_CONNECTOR_ACCOUNTS`. Re-polled every ~30 s,
   so an account a universe admin creates in the UI starts without a
   restart, and a disabled one stops. Per account: `auth/grants/lease`
   for the provider token (a retrievable grant, completing the P133
   "Channels remain open" item — the `LIGHTSPEED_CHANNELS_*_BOT_TOKEN`
   env vars go away), start the poller/socket, start the activity worker
   on the derived delivery queue.
2. **Inbound**: normalize → `channels/inbound/admit { accountId, inbound }`
   stamped with the account's universe → `{ decision: bound | paired |
   pairing_required | pairing_pending | unbound }`. The host sends the
   pairing prompt / confirmation itself, as today; provider ack only
   after the call returns.
3. **Outbound**: `deliver_channel_message`, `prepare_channel_media`
   (download → `blobs/put` into the core CAS), `maintain_channel_typing`
   (heartbeating, cancelled by scope) — unchanged payloads.
4. Health and metrics as today, per account.

The host authenticates the way the Platform backend does: it is a
first-party deployment process, so in `trusted-header` mode it stamps
`x-lightspeed-universe` per call and uses the operator scope for
discovery; in `single` mode the universe is fixed; in `api-key` mode
(deployments without the Platform) discovery is replaced by a static
account list with one universe key each. One token can serve only one
universe — Telegram allows one `getUpdates` consumer per token and
WhatsApp one socket per session, so the "shared account" of today was
always one poller matching a message against every universe's triggers.

Everything a connector must produce or derive is exported: the inbound
and delivery DTOs through the API contract (`channels/inbound/admit`
params) and the workflow contract (activity payloads, delivery-queue and
conversation-id derivations with known-answer vectors, next to the
existing `deliver_emission` vectors). The `ChannelConnector` interface in
`platform/channels/src/contracts/connector.ts` becomes real: `platform/
connectors/` holds `telegram/`, `whatsapp/`, the shared runtime
(lifecycle, health, metrics, rate limit), presentation, and media code —
the provider-specific ~2.5k lines — and nothing else.

Chat access control changes shape. Today `access.control` resolves a
sender to a Platform org role through `channel_identities` and `member`
at message time; the core cannot join Platform tables and the identity
table had no writer. The chat trigger spec gets handle allowlists
instead: `access: { turn: "anyone" | "listed", allowed?: [handle],
controllers: [handle] }`, edited on the trigger like everything else a
bot owns. Provider handles (Telegram user id, WhatsApp JID) are already
in every inbound envelope.

### 8. Credentials

Nothing new is stored. Webhook HMAC secrets, HTTP poll credentials, and
connector provider tokens are auth grants (P133 `retrievable` where a
worker must hold the value); bot and channel records carry grant ids.
Inside the core the broker is called in-process — the Platform's
`GrantLeaseCache` and the bots' process-local lease cache disappear.
Connectors lease through `auth/grants/lease` with their service
principal, as designed.

### 9. What the Platform keeps and loses

Keeps: better-auth users/orgs/memberships, the universe ↔ organization
mapping (`universes.lightspeed_universe_id`), API-key proxying, the web
UI (bot console, wizard, roster, admin channel page), the operator CLI,
Configurator MCP, the demo backend, connectors.

Changes: `routes/bots.ts`, `routes/channel-accounts.ts` become
passthroughs (`engineClientFor(universe).call("bots/…")`) at the same
paths, so the web UI and the demo stubs keep their URLs and only follow
the new response shapes. Bot writes stay owner/admin, channel-account
writes stay platform-admin, enforced by the Platform before proxying as
today.

Loses: `platform/bots` entirely; `platform/channels` everything but the
connector code (moved to `platform/connectors`); `platform/db`'s `bots.ts`
and `channels.ts` schemas and their migrations (the baseline is
regenerated — greenfield); `routes/bot-hooks.ts`, `bot-common.ts`,
`bot-schedules.ts` (`channels-status.ts` stays); the `@temporalio/*`
dependency of the server; the `platform/workers` role dispatcher
(replaced by the multi-account connector host); the `LIGHTSPEED_BOTS_*`
and core-Channels variable groups in `docs/variables.md`.

## Decisions

1. **Bots and channels core are core.** Records, admission, controller,
   conversation workflow, tools, Schedules, tables: Rust, one database,
   one API. P124's non-goal is reversed for this scope only.
2. **Two domain crates**, `bots` and `channels`, hold everything pure;
   workflows in `temporal-workflow`, activities and service in
   `temporal-runtime` (use subfolders in `crates/temporal-workflow/src/workflows` to segregate the parts), tables in `store-pg`. No new binary, no new
   gateway.
3. **Roles per subsystem, one process by default**: `--roles gateway |
   sessions | bots | channels` (all by default), each worker role its
   own task queue with its workflows and activities end to end, then
   `--task-types workflows | activities` as the further split. No
   bot- or channel-specific binary or image.
4. **Temporal Schedules stay** for `schedule` and `poll` triggers; one
   `BotTriggerFireWorkflow` replaces the two TypeScript fire workflows.
5. **Connectors are a TypeScript multi-account host** that speaks to
   the core through `operator/channels/accounts/list` (discovery),
   `channels/inbound/admit` (inbound), and Temporal activities on
   per-account queues (outbound). It reads no database and holds no bot
   or trigger knowledge. Pure-HTTP connectors (no Temporal access) are a
   later option, not this design.
6. **Channel accounts are universe resources** with authored ids and a
   credential grant, managed by the universe's own admins; the host
   serves any number of them from any number of universes. One token
   serves one universe. `channel_identities` and the membership-role
   join are deleted; chat access is handle allowlists on the trigger.
7. **Webhook ingress lives on the core gateway**
   (`POST /hooks/bots/{triggerId}/{token}`), ingest URLs from
   `LIGHTSPEED_PUBLIC_BASE_URL`.
8. **`bots/reconcile` is gone**: `profiles/put` signals affected
   controllers.
9. **Row keys are authored ids**, revisions gate writes, and the wire
   never carries uuids for bots, triggers, or accounts. Session ids and
   the `#N` handle discipline are unchanged.
10. **In-process everywhere inside the node**: activities call the
    service, the service calls the broker and stores; the only
    cross-process protocols left are Temporal signals/activities and the
    public API.

## Deleted

- `platform/bots/**` (≈5.6k lines of source, 3.7k of tests) — ported.
- `platform/channels/src/{workflows,control-plane,activities,ingress,
  policy,identity,contracts/{bridge,tools,channel,search-attributes}}`
  — ported; the rest moves to `platform/connectors`.
- `platform/db/src/schema/{bots,channels}.ts`, migrations
  `0001_channels`, `0002_bots`.
- `platform/backend/src/routes/{bot-hooks,bot-common}.ts`,
  `bot-schedules.ts`; the bot logic in `routes/bots.ts` and
  `routes/channel-accounts.ts` (they become passthroughs).
- `platform/workers` roles `bots-*`, `channels-*`; the Channels search
  attributes and their registration in `scripts/dev/infra/temporal-ensure.sh`.
- The Temporal-patch, the two-hash-implementation split, and the dead
  code noted in the survey (`poll.ts` empty branches, unused imports,
  the `bot_brief_put` config-rebuild inconsistency) are not ported.

## Slices

1. **Foundations** — `crates/bots`, `crates/channels` (records, validation,
   pure logic, stores, in-memory impls, unit tests ported from the
   TypeScript unit suites); `008_bots.sql` and the `store-pg` impls; `api`
   DTOs and the `bots/*` / `channels/*` manifest with CRUD implemented
   (no controller yet); contract export, TS client, Configurator, web
   types regenerated; reserved session-id prefixes.
2. **Bots runtime** — `BotControllerWorkflow`, `BotTriggerFireWorkflow`,
   admission pipeline, bot activities, tool executor, Schedules
   reconciler, hook route, `bots/state/read`, `bots/close` / `delete`,
   `profiles/put` reconcile signal; `crates/temporal-runtime/tests/
   bots_live.rs` porting the eighteen integration scenarios (dedupe,
   budget, descendants, config reconcile, perKey, coalesce, debounce
   wake, steer/append, sidecar, retention, rotation ×3, close,
   Schedules, directory + receipts, carried chat tools).
3. **Channels core** — `ChannelConversationWorkflow`, control plane,
   `channels/inbound/admit`, channel activities, connector DTOs and
   derivation vectors in the workflow contract; `channels_live.rs`
   porting the two conversation scenarios with a fake connector worker;
   `platform/connectors` rewrite of the two workers onto the API +
   activity-worker seam, grant-leased tokens.
   *Connector side done (2026-08-30):* `platform/connectors` is the
   multi-account host of §7 — discovery through
   `operator/channels/accounts/list`, per-account runners with
   grant-leased Telegram tokens and per-account WhatsApp session
   directories, `channels/inbound/admit` with the decision → reply
   mapping, one activity worker per `connectorTaskQueue` (derivation
   exported from `@lightspeed-ai/sdk/workflow` and asserted
   against the contract vector), one health/metrics listener.
   `platform/channels` and `platform/workers` are deleted; `dev.sh
   full` starts one `connectors` process behind
   `LIGHTSPEED_CHANNELS_CONNECTORS`. `platform/bots` survived only
   because `platform/backend` still imported it (deleted in slice 4). The WhatsApp
   group-join pairing announcement was dropped: the core has no
   "is pairing required" query, and the first message in the group
   yields `pairing_required` anyway.
4. **Platform cut-over** — routes to passthroughs, web and demo stubs on
   the generated types, delete `platform/bots` and the Channels core,
   regenerate the Platform db baseline, `platform/workers` to connector
   roles only, `dev.sh` / `stack.mjs` profiles, `docs/variables.md`,
   `README.md`, `AGENTS.md`, `platform/README.md`.
   *Done (2026-08-30) — see the implementation log.*
5. **Follow-ups (separate decisions)** — `SessionOriginKind::Bot` as
   typed provenance so `session/list` finds a bot's sessions without the
   controller query; per-component task queue override; pure-HTTP
   connectors; the `bot:directory` and chat allowlist UI.

## Verification

- Domain crates: unit tests next to the pure logic, ported one-to-one
  from `platform/bots/test/*.test.ts` and `platform/channels/test/*`
  (identities, validation, refusal codes, webhook verification and the
  GitHub projection, CEL filters and routing, rendering budgets, poll
  cursors, activation, access, delivery planning, media validation).
- `cargo test -p api`, `-p temporal-workflow` fail until the contract
  artifacts are regenerated; `npm run check` regenerates every consumer.
- Live: `bots_live.rs` and `channels_live.rs` under the usual
  `--ignored --test-threads=1` discipline against the local stack,
  including a Schedules round trip and history replay where the SDK's
  replayer supports it; `workflow_tool_plugins_live.rs` keeps its
  simulated-Channels cases (they exercise the generic protocol, not the
  port).
- Dogfood: a Telegram connector against `./dev.sh full` with a chat
  trigger, a webhook trigger fed by GitHub, a poll trigger, and a two-bot
  federation exchange — the live dogfoods still open from P135, P139,
  and P140 land here.

## Risks

- `temporalio_client::schedules` and `ActivityOptions::task_queue` are
  first uses in this repository; both exist in 0.4.0 and were checked,
  but ergonomics gaps are likely and belong in slice 2 / 3, not later.
- The controller's concurrency (lanes, sidecars, ticks) must be rebuilt
  on `select_all` over boxed futures; the TMPRL1100 constraint on custom
  wakers applies, and only query-during-batch live tests catch it.
- CEL parity: `cel-interpreter` (cel-rust) versus `cel-js` differ at the
  edges (macros, string functions). Filters are validated at save time
  and evaluated fail-closed, so a gap surfaces as a refused put or a
  stored filter error, never a silent delivery.
- Universe-scoped channel accounts mean a provider account cannot serve
  two universes; the admin page and the Platform CLI must pick a
  universe when creating one. The multi-account host is new code in
  `platform/workers` (N pollers, N activity workers, discovery refresh,
  per-account health) — the connector code itself is unchanged.

## Non-goals

- Moving users, organizations, memberships, the web UI, the operator CLI,
  or Configurator MCP into Rust.
- Making connectors Temporal-free, or running them outside the
  deployment.
- Bot- or channel-specific binaries or images, or a second gateway for
  bots or channels.
- Any compatibility path for existing Platform rows, workflow histories,
  Schedules, or connector configuration.

## Implementation log

2026-08-30, slices 1–3 in one pass:

- **Foundations.** `crates/api/src/{bots,channels}.rs`: 26 universe methods
  (`bots/*` ×17, `channels/*` ×9) plus `operator/channels/accounts/list`
  (120 methods, 15 operator); `bots/sessions/rotate` is the one non-`session/`
  method carrying a `sessionId` (it addresses the bot). `crates/bots`
  (records, store traits, in-memory store, validation with save-time CEL and
  cron parsing, filter/route, webhook verification + GitHub projection,
  rendering, poll cursors, tool declarations, views, identities; 125 unit
  tests) and `crates/channels` (accounts, pairings, inbound, policy,
  delivery plans, media, conversation state, `message_*` declarations; 55
  tests). `store-pg` migrations `008_bots.sql` (`bots`, `bot_triggers`,
  `bot_events`) and `009_channels.sql` (`channel_accounts`,
  `channel_pairings`) with the store implementations and four live pg
  tests. Contract artifacts and the
  TypeScript client regenerated; the workflow contract now exports
  `ConversationStart`, the connector activity payloads, the connector queue
  derivation, and channel vectors.
- **Runtime roles.** `lightspeed-runtime [--roles gateway,sessions,bots,channels] [--task-types all|workflows|activities]`
  replaces the `gateway`/`worker`/`both` subcommands; per-role queues
  `LIGHTSPEED_TASK_QUEUE` (`lightspeed-sessions`), `LIGHTSPEED_TASK_QUEUE_BOTS`,
  `LIGHTSPEED_TASK_QUEUE_CHANNELS`; the bot schedule reconciler runs beside
  the environment reconciler and power reaper.
- **Bots runtime.** `BotControllerWorkflow` (lanes as boxed futures polled
  under the loop, no custom wakers; 32 unit tests) and one
  `BotTriggerFireWorkflow`; `temporal-runtime/src/bots/`: store-then-wake
  admission with row compensation, Temporal Schedules through the raw
  workflow service (the typed client cannot carry workflow input), the
  controller's session activities, the pushed `bot_*` executor, receipts and
  the directory, schedule/poll fires (HTTP with in-process grant leases,
  exec through environment jobs), the public hook route
  `POST /hooks/bots/{universe}/{bot}/{trigger}/{token}`, `profiles/put`
  signalling open bots, and reserved session-id prefixes.
- **Channels core.** `ChannelConversationWorkflow` (12 unit tests),
  `channels/inbound/admit` control plane (candidates by priority, open /
  paired / code / prompt decisions, signal-with-start), the core-side
  activities, and the connector seam: three activities on
  `lightspeed-connector-{provider}-{digest}` served by the TypeScript
  connector host in `platform/connectors`.
- **Live proof** (`cargo test -p temporal-runtime --test bots_live|channels_live -- --ignored --test-threads=1`):
  manual event → run → outcome, duplicate admission keeps `#N`; webhook
  trigger with filter and coalescing into one batch delivery, filtered and
  probed requests refused; daily budget parking; Temporal Schedule create /
  manual fire / pause on bot disable / delete with the trigger; close
  (archive, force-close, recorded sessions, refused events) and delete; a
  real OpenAI model resolving a delivery `handled`; a chat message admitted
  through `channels/inbound/admit` reaching the bot's routed session and its
  reply going out through the connector queue with the `chat.sent` row
  archived; pairing required → paired → bound → unpaired.
- **Schema review (2026-08-30, after the first pass).** `008_bots.sql` split
  into bots (008) and channels (009), `REQUIRED_SCHEMA_REVISION` → 9, both
  files regrouped (identity / operator document / runtime-owned; the event
  log's five groups) with per-column documentation. `bot_events` lost
  `source` (the source is the trigger or the sender bot) and `delivery_id`
  (deliveries live in the controller's history; `run_id` is the durable
  handle), and `reply_to_json` + `notify_json` + `tools_ref` merged into one
  private tagged `receiver_json` (`EventReceiver`: `workflow` with receipt
  token and receiver-bound tools, or `bot` with the asker's logical
  session). `ChannelProvider` became an open, format-checked name — the
  core never enumerates providers, so a new channel type is a connector
  concern, not a core change — with provider-specific account settings in
  an uninterpreted `settings` extension map, and
  `channel_accounts_provider_account_unique` became deployment-wide: one
  provider account belongs to exactly one universe, refused at create with
  a typed error instead of failing later as two runners fighting over one
  token.
- **Pairing-first routing (2026-08-30, after the channels review).**
  Pairing is the routing authority: `plan_admission` checks the pairing
  row first and a paired chat routes only to its paired trigger — a
  disabled owner parks the chat (`unbound`, silence) instead of losing it
  to an open trigger; an unpaired chat is claimed by the best open trigger
  (`Claim` writes the pairing row, wire decision stays `bound`) or by
  pairing code; the conversation active-check requires the pairing to
  point at the serving trigger for every pairing mode, so unpairing or
  re-pairing ends the old conversation workflow. Priority is now only the
  tie-breaker among claimants at first contact. Decided with Lukas after
  weighing deployment-scoped accounts (rejected for now: the inbound
  wire→tenant demux would be the gateway's first data-derived tenancy
  decision; revisit as an additive account scope if shared numbers become
  real demand).
- **Channels tables review (2026-08-30).** `channel_pairings` lost its
  stored digest: `pairing_key` is gone, the primary key is the
  conversation itself (`universe_id, account_id, chat_id`), the store and
  `channels/pairings/delete` address pairings by `(accountId, chatId)`,
  and the derivation left the workflow contract. A `paired_via` column
  (`open` | `code`) records how the chat got its route now that open
  triggers claim silently. `channel_accounts` gained the
  matches-document CHECKs (`provider`, `provider_account_id`) that
  `bot_triggers.kind` already had.
- **Naming pass (2026-08-30).** The two core `workflow_kind` values are
  the registered workflow type names (`BotControllerWorkflow`,
  `ChannelConversationWorkflow` — previously `bots.controller` vs the
  type name); test fixtures and the recipe contract vector stopped using
  the TS-era `botControllerWorkflowV1` / `channelConversationWorkflowV1`
  strings and the retired `lightspeed-bots-workflows-v1` queue name;
  AGENTS.md now names the real workflow type and the `bot_delivery`
  signal.
- **Deviations recorded while porting:** the filter's `event.occurredAtMs`
  is an integer; poll cursors advance only over delivered items (the TS
  silently dropped items past the per-fire cap); `resolve_inbox` reports a
  disabled inbox as `trigger_disabled`; the inbound dedupe key / receipt
  token uses U+001F as separator (jsonb rejects U+0000); chat event
  documents carry `data.conversation` / `data.message` (CEL filters written
  against the TS layout need updating).
- **Slice 4, Platform cut-over (2026-08-30).** `platform/bots` is deleted
  and the Platform backend is no longer a Temporal client
  (`@temporalio/client` and `@lightspeed-ai/bots` left its dependencies).
  `routes/bots.ts` and `routes/channel-accounts.ts` are thin passthroughs
  on the existing `engineClientFor` / `withGateway` seam at the same URL
  paths with the core response shapes: reads keep member access, writes
  keep owner/admin, and the platform still strips `ingestPath` /
  `pairingCode` from trigger views for non-managing members (only the
  platform knows org roles). PATCH became put-with-expected-revision end
  to end; `POST /bots/reconcile` and the platform webhook mount are gone
  (`profiles/put` reconciles in core; ingest URLs are the core gateway's
  `/hooks/bots/...`). Channel accounts moved under
  `/universes/{id}/channel-accounts` (+ pairing routes); the
  platform-admin listing is the core's `operator/channels/accounts/list`;
  the operator CLI grew `--universe` and read-modify-write
  enable/disable. The platform database lost `schema/{bots,channels}.ts`
  and migrations `0001_channels` / `0002_bots` — the ledger rebased to
  the single `0000_platform_baseline`
  (`LIGHTSPEED_PLATFORM_SCHEMA_REVISION=1`), and the migration gate now
  asserts the moved tables never reappear (the `lightspeed_channels`
  role machinery went with them). The web UI and the demo backend read
  the generated `@lightspeed-ai/sdk` types (`platform/web/src/api.ts`
  re-exports them; the demo emulates the new wire exactly, hooks at the
  core-shaped path). Release staging no longer ships `platform/bots/src`.
  Found while migrating: `BotTriggerView` serialized the poll spec's
  dedupe `cursor` and the advancing `PollCursorState` under one JSON key —
  the runtime field is now `cursorState`. The chat access UI moved to the
  handle-allowlist `ChatAccess` shape (the org-role model died with
  `channel_identities`). Post-cut-over fix: the demo engine's user entries
  now carry `source` (`runInput` / `steering`) and `runAccepted` carries
  `submissionId`, like the live projection — without them the page cannot
  reconcile its optimistic bubble and shows it twice until the run ends.
- **Universe-owned channel setup (2026-09-01).** Provider-account setup moved
  from the platform-admin surface to a Channels page in each universe's
  Settings. The normal Telegram flow accepts the BotFather token, validates it
  with `getMe`, derives the provider identity and authored account id, imports
  the retrievable grant, and compensates by revoking that grant if account
  creation fails; grant ids are no longer user input. WhatsApp setup similarly
  derives its account id from the phone number. The page owns enable/disable,
  per-account connector health, pairing visibility, and unpairing; bot pages
  still own chat-trigger behavior. Platform Admin → Channels is now a read-only
  deployment inventory and connector diagnostic surface. The demo backend
  mirrors the connection and universe-health routes.
