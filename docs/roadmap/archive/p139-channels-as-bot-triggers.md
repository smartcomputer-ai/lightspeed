# P139 — Channels as Bot Triggers: Chat Connections Route to Bots

**Status**

- **Implemented 2026-08-26**, all four slices in one pass; see the
  implementation notes at the end of the Slices section.
- Proposed 2026-08-26 from a design conversation with Lukas, after
  [P130](p130-bots.md) slices 1–4, [P134](p134-subagents.md), and
  [P135](p135-bot-federation.md) landed. Replaces the deferred "Channels
  bridge" item of [P131](p131-bot-trigger-long-tail.md) §5, and goes one
  step further than its settled shape.
- Decisions taken in that conversation, in order:
  1. Channel routing should target a **bot**, never a bare managed session.
     The first draft kept a `session | bot` target discriminator on
     `channel_bindings`; Lukas: "a channel routing should always tie to a
     bot". Adopted, and taken to its conclusion: **a chat connection is a
     bot trigger of kind `chat`**, and `channel_bindings` is deleted.
  2. `perKey` (one bot session per conversation) is the starting point.
     `bot`-routed chats (a shared main session with reply tools) are a
     later slice.
  3. Chat sessions do not expire — a per-trigger `sessionCloseAfterMs` override
     of the bot's `routedSessionCloseAfterMs`, `null` for chat.
  4. Inbound media stays at parity with Channels today (image, audio,
     document; up to 8 per message); the event carries the prepared CAS
     refs.
  5. `silent` rooms (room context without runs) are out of v1; the
     mechanism is cheap (§9) and lands when someone misses it.
  6. Greenfield: the old channel-owned session path is deleted outright,
     no compatibility layer.
  7. Delivery receipts to the conversation workflow are a separate optional
     `notify` field on the event, not a second variant of P135's `replyTo`
     (Lukas, same day): `replyTo` stays "a bot inbox through admission",
     `notify` is "a workflow endpoint through a signal".
  8. Message handles reviewed against [P138](p138-model-facing-ids.md)
     (same day, Lukas asked for a philosophical check): Channels hands
     the model provider message ids — WhatsApp's are 16–32 hex — and
     prints the chat JID in every envelope. Decided to simplify now
     rather than later: handles are the bot's `#N` in both directions
     (§4b), no provider id or JID in front of the model.
- Builds on P100/P100b/P106 (workflow tools, emissions, receivers),
  P103 (managed sessions), P130 (admission pipeline, routed sessions,
  coalescing, delivery policies), P135 (receipts), P136 (catalogs),
  P138 (model-facing ids), and the Channels application (providers,
  pairing, activation, delivery).

## Why

Bots are "a proactive layer over managed sessions" and P130 named them
"eventually the umbrella over Channels". Today they are not: a Telegram or
WhatsApp conversation is bound by a `channel_bindings` row to a session that
the channel workflow itself creates and controls, and a bot cannot be
reached from a chat at all. The two applications implement the same
controller shape twice — `ensureManagedSession`, run start, terminal
reconciliation, batching, continue-as-new — and only one of them has a
brief, a budget, an event log, filters, a breaker, and federation.

The core already draws the line this design needs: a session has exactly
one lifecycle controller, but every bound workflow tool names its own
receiver, and receivers have no lifecycle authority. So the bot controller
can own the session while the channel workflow keeps answering
`message_send`. Nothing in the engine or the API changes.

## The shape

```text
Telegram / WhatsApp
      │ connector (normalize, media refs)
      ▼
 conversation workflow ── admitTriggerEvent ──▶ bot controller ──▶ bot:v1:<bot>:k-<chat>
 (one per chat; Channels)   one event per        filter · route ·      session
      ▲        ▲            message              coalesce · budget        │
      │        └──────── receipts: started / finished ◀───────────────────┤
      └─────────────── message_send / edit / react / noop (pushed by the core session)
```

- **Channels owns providers**: accounts, pairing, member authorization,
  activation classification, control commands, media download, delivery,
  typing, and provider message ids. It is a *source* (every message is a
  bot event) and a *receiver* (the `message_*` tools of that conversation).
- **Bots own sessions**: one admission pipeline, routed sessions, brief,
  budget, breaker, event log, `#N`, receipts, federation.
- The bot controller never sees a `message_*` call. The core pushes the
  invocation to the receiver the declaration names — the conversation
  workflow — which delivers and resolves the reply promise, exactly as
  today. The only bot ↔ Channels traffic is the event going in and the
  delivery receipt coming back.

## Design

### 1. The `chat` trigger

`bot_triggers.kind` gains `chat`. The spec is what `channel_bindings`
carried, minus the two columns the bot makes redundant (`profileId` is the
bot's; `sessionKey` is the route):

```ts
interface ChatTriggerSpecV1 {
  channelAccountId: string;              // platform channel_accounts row
  matchScope?: "direct" | "group" | null;
  activation?: { group?: "mention" | "always"; triggerPrefixes?: string[]; mentionNames?: string[] };
  access?: { turn?: "conversation" | "members"; control?: "none" | "members" | "admins" | "owners" };
  pairingCode?: string | null;           // null = open (pairs implicitly); minted when omitted
  priority?: number;                     // lower wins among matching triggers
}
```

The generic trigger columns apply unchanged and are the whole point:

- `filter` — CEL over `{event, data, headers}`; `data.sender.id`,
  `data.text`, `data.isDirect`, `data.mentionedBot` are all there.
- `route` — **`perKey` by conversation in v1**, key `data.conversation.key`
  (provider + account + chat + thread). `perEvent` is allowed; `bot` is
  refused until slice 4.
- `coalesce` — replaces Channels' own batching. Defaults are today's
  `{ debounceMs: 400, maxWaitMs: 1500, maxCount: 8 }`.
- `deliver.whenBusy` — `queue` by default; `steer` folds a new message into
  the running turn.
- New, all kinds: `sessionCloseAfterMs: number | null | undefined` — overrides
  `bots.routedSessionCloseAfterMs` for sessions this trigger routes to;
  `undefined` inherits, `null` never closes. Chat defaults to `null`.

`channel_pairings.bindingId` becomes `triggerId`. Several `chat` triggers
per bot are fine (different accounts or scopes); per-conversation receivers
never collide.

The trigger is created from the bot page (kind-aware form, pairing code
shown once as the webhook ingest URL is) and, under `selfConfig`, by the
bot itself with `bot_trigger_put kind=chat` — a bot can pair its own chat
by telling the human the code.

### 2. The conversation workflow

`channelSessionWorkflowV1` becomes `channelConversationWorkflowV1`: the
current workflow minus everything that made it a session owner.

Keeps: `signalWithStart` identity per (universe, provider, account, chat),
inbound dedupe, activation classification, control commands (`/activation`,
`/status`), denied-sender and pairing replies, `prepareChannelMedia`,
`replyTargets`, the delivery task queue, typing, `message_*` invocation
handling, promise resolution, continue-as-new.

Drops: `ensureManagedSession`, `startChannelRun`, `pendingTurns` and the
batch deadline, room context append/prune, the lifecycle-controller role,
and `sessionId`/`sessionKey`/`profileId` in its start argument.

Adds: an `emitChatEvent` activity (§3), a `bot_delivery_v1` signal for
receipts (§5), and a set of session ids it accepts pushed invocations from
— the ids admission returned for its emits, since routed sessions rotate
generations.

`/status` reports the bot id instead of a session id. The control-plane
resolver reads `bot_triggers where kind = 'chat'` joined to
`channel_accounts`, `bots`, and `universes`, ordered by priority; the
pairing plan is unchanged.

### 3. Inbound: one event per message

After classification yields a user turn, the conversation workflow runs
`admitTriggerEvent` (the shared pipeline: breaker → filter → route →
coalesce → delivery policy → store-then-wake) with:

- `eventId`: `chat:<triggerId>:<providerMessageId>` — deterministic, so a
  provider redelivery or an activity retry converges on one row.
- Document `kind: "chat.message"`, `source: "<provider>:<accountId>"`,
  `data: { conversation: { key, provider, chatId, threadId?, scope }, sender:
  { id, name, memberRole }, messageId, text, isDirect, mentionedBot,
  isReplyToBot, media: [{ kind, mime, name }] }`.
- `promptData`: the chat preset projection — `Alice (2026-08-26 14:02Z):
  text`, followed by `[image: photo.jpg]` labels, under the renderer's own
  `event #17 chat.message from telegram:acme-support` header. No provider
  message id and no chat JID in front of the model: `#17` is the handle
  (§4b); `chatId`, `messageId`, and `threadId` stay opaque provider data
  in `data`, reachable by filters and `bot_event_read`.
- `media`: the prepared `{ blobRef, kind, mime, name }` items (§7).
- `tools`: the CAS ref of the receiver-bound declarations (§4).
- `notify`: this workflow's endpoint and a token for delivery receipts (§5).

The provider is acknowledged only after admission resolves, the same
durability guarantee `signalWithStart` gives today. Channels-side batching
is gone; the trigger's `coalesce` batches, and a coalesced delivery renders
as today's bot batch (one header, N events, one resolution).

### 4. Receiver-bound tools travel with the event

Tool declarations are immutable per session and can only be given at
`session/managed/start`, which only the bot controller calls. So Channels
authors the declarations and bots pastes them in:

1. Channels already puts the `message_*` schemas and descriptions into the
   universe CAS (`putToolAssets`). It builds `channelWorkflowTools(receiver
   = its own workflow endpoint, …)` — four declarations, `dispatch: push`,
   joined with the 120 s deadline (`noop` accepted-pull) — writes the array
   to CAS, and puts the ref on the event as `tools`.
2. `ensureRoutedSession` passes `[...botWorkflowTools(controller),
   ...declarationsAt(tools)]` to `session/managed/start`. Bots treats the
   ref as opaque data: it checks only that names do not collide with
   `bot_*` and that every event for one routed session carries the same
   ref. The latter holds by construction — same conversation, same
   receiver, same bytes, same creation fingerprint — and a mismatch takes
   the existing rotate-to-`-g2` path.
3. From then on the core routes: `message_send` invocations are pushed to
   the conversation workflow; `bot_event_resolve` is pulled by the
   controller; `run_terminal` goes to the controller. Neither side sees
   the other's tools.

`BotEvent` (the Temporal inbox value) gains `tools?: string` (blob ref) and
`media?: […]` (≤ 8 small refs); `bot_events` stores both so replay
re-creates the same session shape.

### 4b. Message handles are `#N` in both directions

Channels today hands the model *provider* message ids as the copy-back
handle — `replyTo`, `message_edit.messageId`, `message_react.messageId`,
and the send receipt's `messageIds`. Telegram's are small integers;
WhatsApp's are 16–32 uppercase hex, exactly the digest shape
[P138](p138-model-facing-ids.md) removes elsewhere, and the bots
tool-views test would reject them in a rendering. Provider ids also carry
no direction, which is why WhatsApp `message_react` hard-codes
`fromMe: false` and cannot react to the bot's own message.

The fix follows P138's rule — the model copies counters, the conversation
workflow owns the mapping — and reuses the number the bot already has:

- **Inbound**: one message is one event, so the bot's `#N` is the handle.
  Admission returns `seq`; the conversation workflow records `#N →
  { providerMessageId, fromMe: false }` in its bounded carry (today's
  `replyTargets`, re-keyed).
- **Outbound**: the bot's own send is stored as an archived `chat.sent`
  event on the same bot — `deliver: false`, like a filtered event: in the
  log with a `#N`, never delivered — carrying the text, the provider ids
  (several for a chunked send), and `fromMe: true`. The send receipt is
  `{ sent: 18 }`. The bot's event log thereby reads as the whole
  conversation ledger, and `bot_event_read 18` works on sends too.
- **Tools**: `message_send { text, replyTo?: integer }`, `message_edit
  { message: integer, text }`, `message_react { message: integer, emoji }`.
  The workflow resolves the handle to the provider id and direction; an
  unknown handle is a typed, retryable tool error naming the valid range.

One namespace per bot: `replyTo: 17`, `bot_event_read 17`, the event log,
and the web UI all mean the same message.

### 5. Receipts and the reply fallback

The controller owns `run_terminal` now, so the fallback Channels performs
today ("no messaging tool was used — send the assistant's text") needs a
receipt. It is a separate, optional field on the event — not a variant of
P135's `replyTo`, which stays bot vocabulary (a bot inbox reached through
admission):

```ts
interface BotEventNotify {
  workflow: { workflowId: string; workflowKind: string };
  token: string;   // opaque to bots; echoed on every receipt
}
```

`notify` is set by the admitting source, stored on `bot_events` (private,
never on the wire or in front of the model), and carried on the inbox value
as `notify: true`. When a delivery holds an event with `notify`, the
controller signals `bot_delivery_v1` on the named workflow — never an
event, never admission — at two points of the lane, next to where
`settleReceipts` already runs:

- `started { token, deliveryId, sessionId, runId }` when the run starts
  (typing begins here, not at emit: coalescing and queue wait would make
  "typing…" lie).
- `finished { token, deliveryId, sessionId, runId, status, outcome, summary }`
  when the delivery finishes with any status (`run_completed`, `run_failed`,
  `steered`, `appended`).

On `finished` the conversation workflow runs today's `reconcileTerminalRun`
against `sessionId`/`runId`: suppress if the run used a `message_*` tool,
otherwise send the assistant text (or the failed/cancelled line); stop
typing either way. `steered`/`appended` receipts only stop typing.

A run that used a source-bound tool resolves its delivery `handled` when the
model did not call `bot_event_resolve` — two bookkeeping calls per chat turn
is too much ceremony. Explicit resolutions still win.

### 6. Sessions and retention

Every chat session is a routed bot session, `bot:v1:<bot>:k-<slug>-<digest>`
with the conversation as key, display name `<bot> · telegram: <chat label>`,
the bot's profile, brief, and tools, plus that conversation's `message_*`.
It appears in the bot page's session list and in the sessions page like any
managed session (the Direct-input override applies).

`sessionCloseAfterMs` on the trigger (§1) keeps chat sessions open indefinitely by
default; the routed-session sweep consults the trigger of the event that
opened the session. There are no `channel:v1:` sessions any more.

### 7. Media

Parity with Channels today, and no new machinery: `prepareChannelMedia`
already runs on the provider task queue (it has the credentials) before a
turn is queued, downloads the attachment, validates kind/MIME/size, and
puts the bytes into the universe CAS, returning `{ type: "media", blobRef,
kind, mime, name }`. The event carries those items; `deliveryInputItems`
and `steerInputItems` append them after each event's rendering as run input
`media` items, so the model receives the image/audio/document exactly as it
does through the channel session now. Outbound stays text (`send`, `edit`,
`react`).

### 8. Rules

- A chat connection is a `chat` trigger on a bot; the bot must exist first.
  Channels never creates sessions, starts runs, or holds a lifecycle role.
- Every message is one admitted event; every reply is a pushed `message_*`
  invocation or a receipt-driven fallback. No other path.
- Receiver-bound declarations are opaque to bots and identical for a routed
  session's lifetime; a change rotates the session.
- `notify` receipts are signals with a token, never events; `replyTo`
  receipts are events through admission, never signals. They are
  sent by the controller when the lane changes state, never by the model.
- Model-facing ids stay P138-clean: the model sees `#N` for every message,
  inbound and sent, and `message_send { text, replyTo: 17 }`; never a
  provider message id, chat JID, session id, workflow id, or route hash.
  The conversation workflow owns the handle → provider id mapping.

### 9. What `silent` would take (not in v1)

A room event is an event with delivery policy `append` — the controller
already appends without running. Missing is retention: the controller
remembers appended context keys per session (bounded, in carry) and
`session/context/remove`s the oldest beyond `roomContextLimit`. ~50 lines,
and it also fixes a latent gap — any `append`-policy trigger grows its
session's context unbounded today. Lands when someone wants silent rooms.

## What is deliberately not built

- A `session | bot` target on bindings, or bindings at all.
- Two controllers or co-management (P100 non-goal; the core enforces one).
- A channel router workflow as a single receiver with routes in tool
  arguments — it would put route ids in front of the model.
- Channels-side batching in parallel with trigger coalescing.
- Outbound media.
- A migration of existing bindings; dev bindings are recreated as triggers.

## Slices

1. **Records and admission.** `chat` trigger kind (schema, zod, trigger
   CRUD and `bot_trigger_put`, validation: `perKey`/`perEvent` only,
   pairing-code mint), `sessionCloseAfterMs` on all kinds, `channel_pairings` →
   `triggerId`, delete `channel_bindings` and the bindings API. `BotEvent`
   / `bot_events` gain `media`, `tools`, and `notify`; `storeBotEvent`
   accepts archived `chat.sent` rows from the conversation workflow
   (§4b). One migration; platform schema revision bump.
2. **Bots generic pieces.** `ensureRoutedSession` merges carried
   declarations; `deliveryInputItems`/`steerInputItems` append media;
   endpoint receipts `started`/`finished` from the lane; implicit
   `handled`; per-trigger TTL in the sweep. Integration scenario: a routed
   session created with carried tools, receipts observed by a fake
   endpoint, history replay.
3. **Conversation workflow.** Rename and cut `channelSessionWorkflowV1`;
   control plane over `chat` triggers; `emitChatEvent` activity into
   `@lightspeed-ai/bots` admission; receipt signal handler with the existing
   fallback; accepted-session set for invocations; `#N` handle map in the
   carry, integer `replyTo`/`message` in the `message_*` schemas
   (revision 2), `chat.sent` rows on every delivered send, and the
   WhatsApp react/edit key taking `fromMe` from the handle. Integration scenario
   (fake delivery): pair, turn → event, `message_send` reply, fallback
   text on a tool-less run, media items on the run input.
4. **UI and follow-ups.** Chat trigger form on the bot page (account,
   scope, activation, access, pairing code, TTL); Channels page keeps
   accounts and status. Then, on demand: `bot`-routed chats (a paired
   conversation's tools on the main session, at most one per bot — the
   "DM me the digest" case), `silent` rooms (§9), thin Slack preset.

### Implementation notes (2026-08-26)

- **Records**: folded into the pre-release `0001_channels` / `0002_bots`
  baselines (platform schema revision 3): `channel_bindings` is omitted,
  `channel_pairings` is keyed by `trigger_id`, and
  `bot_triggers.session_close_after_ms` (null inherits, 0
  never closes) and `bot_events.media` / `tools` / `notify`. The `chat`
  trigger kind lives in `platform/bots/src/config.ts` (`chatSpecInput`,
  `CHAT_COALESCE_DEFAULT`, `mintPairingCode`; route `bot` refused, coalesce
  defaults to 400 ms / 1.5 s / 8, `sessionCloseAfterMs` defaults to 0). The
  bindings API, CLI commands, and shared zod schemas are deleted; the
  server attaches `channelAccount` to chat trigger views.
- **Bots**: `BotEvent.media` / `tools` / `notify` and
  `BotEventSession.ttlMs` (`contracts/bots.ts`, validated);
  `admitTriggerEvent` passes them through and applies the `chat` route
  preset (`data.conversation.key` / `.label`, `bot` forced to `perKey`);
  `ensureBotSession` merges carried declarations after a collision check
  (`validateCarriedDeclarations`) and reports `carriedToolIds`;
  `deliveryInputItems` / `steerInputItems` append media items;
  `readWorkflowToolInvocations` returns every bound-tool invocation;
  `sendDeliveryReceipts` signals `bot_delivery_v1` per (workflow, token);
  the controller sends `started` after `startBotRun` and `finished` from
  `rememberDelivery`, treats a carried-tool call as `handled`, and sweeps
  by per-session TTL. `bot_trigger_put kind=chat` takes
  `channelAccount: provider:accountId`; `bot_trigger_list` shows the same
  handle and, under `selfConfig`, the pairing code. Tools revision 10.
- **Channels**: `channelConversationWorkflowV1` (`workflows/conversation.ts`)
  replaces the session workflow: identity per (universe, provider,
  account, chat, thread) with a readable `conversationKey`; control plane
  over `bot_triggers where kind = 'chat'`; `emitChatEvent` /
  `storeChatSent` / `resolveChatHandle` bridge activities
  (`activities/bot-bridge.ts`) over the bots admission package;
  `putChatToolDeclarations` stores the receiver-bound `message_*`
  declarations (revision 2, integer handles, `{ sent: N }` / `{ message: N }`
  receipts); `reconcileDelivery` keeps the text-reply fallback; WhatsApp
  reactions take `fromMe` from the handle. The event summary is the
  message line `Alice (14:02Z): text`; provider ids live only in `data`.
  Silent rooms are gone (ambient group traffic is dropped).
- **Web**: chat trigger kind on the bot page (account, scope, activation,
  access, pairing with copy/rotate, retention, delivery fields); the
  universe "Channels" settings page and its menu item are removed —
  provider accounts stay on the admin Channels page, connections live on
  each bot, and members land on the bots list.
- **Verified**: bots unit (89) and Temporal integration (15 scenarios,
  incl. carried tools + receipts + implicit handled + never-expiring chat
  session, with replay); channels unit (97) and Temporal integration (2
  scenarios: media + numbered send + fallback + duplicate receipt; group
  activation + control commands + foreign-session refusal, with replay);
  web typecheck/tests/build; `test:migrations` fresh + upgrade;
  `npm run check`. Not yet run: a live Telegram/WhatsApp chat on the dev
  stack.

### Follow-up 2026-08-26: outcomes on the event row, no activity feed

Lukas asked why bots keep a separate `bot_activity` table when the
session log exists, and whether admitting filtered events is right for a
firehose. Both were overdone; both simplified the same day (folded into
`0002_bots`, platform schema revision 3):

- **`bot_activity` is gone.** The bot's decisions live in the controller's
  Temporal history; Postgres is a read model. Every `bot_events` row now
  carries a write-once `outcome` (the model's `handled` … `blocked`, or the
  system's `unresolved | run_failed | steered | appended | archived`;
  `NULL` = pending), `outcome_detail`, `delivery_id`, `run_id`,
  `resolved_at`, written once per event when the delivery finishes — so a
  coalesced batch marks all its rows. Trigger-level incidents became trigger
  state: `bot_triggers.disabled_reason` (`breaker | poll_failed | operator`)
  with `disabled_at`, and `last_filter_error` / `_at` for a CEL filter that
  throws (fail-closed; cleared by the next match). Controller-level facts
  (degraded, rotation, budget) stay in the live snapshot. The Activity tab
  and `GET …/activity` are removed; the Events tab shows outcomes inline.
- **Filter misses are never stored.** No seq, no CAS write, no row: a strict
  filter on a firehose costs nothing, and `#N` never skips for junk. The
  event log keeps only what the bot saw. `bot_filter_test` therefore takes
  a `payload` (`{kind?, data?, headers?}`) to write a filter before any
  traffic exists; without one it still samples the stored (delivered)
  events, which is enough to tighten a filter that is too loose.

## Tests

- **Unit** (`platform/bots/test`): chat event id determinism; the chat
  preset rendering shows sender, time, text, and media labels, no provider
  id or JID, and passes the no-digest/no-uuid assertion over Telegram and
  WhatsApp fixtures (including a `chat.sent` row); carried-declaration merge
  (collision refused, identical ref accepted); receipt payloads per
  finish status; TTL override resolution.
- **Unit** (`platform/channels/test`): control-plane selection over chat
  triggers (priority, scope, pairing states); declaration authoring
  (receiver = own endpoint, stable bytes); invocation acceptance over the
  rotated-session set.
- **Integration**: the two scenarios in slices 2 and 3;
  `test:migrations` asserts the deleted and added columns on fresh and
  upgrade paths; `npm run check` and `check:identity` green.
- **Live**: pair a Telegram chat to a bot on the dev stack, send text and
  an image, see the `#N` in the bot event log, the reply through
  `message_send`, and the fallback when the brief forbids tools.
