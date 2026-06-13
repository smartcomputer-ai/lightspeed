# P71: Messaging Channel Gateway, Delivery Outbox, And Media Input

**Status**
- Proposed 2026-06-12.
- G1 and G2 implemented 2026-06-12; G3 first cut (image input) implemented
  2026-06-12; G4 and G5 first cut (outbox, messaging toolset, tools-first
  delivery with final-text fallback) implemented 2026-06-12 (see goal
  sections for what shipped).
- G3 second cut (document input: PDF + text-based documents) and bridge
  typing indicators implemented 2026-06-12.
- Builds on the P70 external integration surface (schema export, TS client,
  idempotent `run/start`, long-poll `session/events/read`), the first-cut
  Telegram/WhatsApp bridge in `interop/messaging/`, and the timers/triggers
  proposal in `p101-timers-schedules-and-triggers.md`.
- Inspired by OpenClaw's channel gateway (activation modes, ambient room
  context, queue discipline, tool-based outbound messaging), adapted to
  Lightspeed's deterministic engine and hosted runtime.
- Naming note: some older docs (`p63-skills.md`,
  `p101-timers-schedules-and-triggers.md`) reference "P71 prompt management";
  that work shipped as P65. This document claims the unused P71 slot.

## Goal

Evolve `interop/messaging` from a slash-command request/reply adapter into a
**session-bound channel gateway**, and give Lightspeed the missing primitives so an
agent session can participate in chats the way a person does:

- **Listening** — ingest allowed Telegram/WhatsApp traffic continuously.
  Direct messages and mentions become normal run input; unaddressed group
  chatter becomes session context, not turns.
- **Deciding** — per-binding activation and delivery policy decides when a
  turn runs and whether its output is visible. The agent's own judgment is
  expressed through an explicit send tool, not prompt tokens.
- **Speaking** — a durable, channel-neutral delivery outbox through which
  final replies, agent-initiated tool sends, and (later) trigger
  announcements all reach channels. A Lightspeed session can initiate messages
  without waiting for an inbound chat message.
- **Media** — images, documents, and voice notes enter sessions as
  CAS-blob-backed input items; audio is transcribed server-side.

The end state: slash commands remain a control surface (`/activation`,
`/new`, `/status`), but the primary interaction model is "the bot is present
in the chat, knows what is going on, replies when addressed, speaks up on its
own when it has a reason to".

## Design Position

**Split listening, deciding, and speaking.** The current bridge couples one
inbound message to one run to one outbound reply. That shape cannot support
ambient group presence or agent-initiated messages. Inbound handling, turn
policy, and outbound delivery become three independent layers that meet at
the session log.

**The Lightspeed session transcript is the source of truth.** Channels attach to
sessions through bindings. Telegram and WhatsApp do not mirror each other;
they are independent delivery surfaces for the same or different sessions.

**Delivery policy, not prompt tricks.** Whether output reaches a chat is
decided by per-binding policy plus an explicit `message_send` tool call —
auditable, rate-limitable, testable. No `NO_REPLY` / `HEARTBEAT_OK` sentinel
tokens as the core mechanism (OpenClaw's newer structured-tool direction,
not its legacy token contracts).

**Room events are context, not turns.** An LLM run per unaddressed group
message is a cost and noise disaster. Unaddressed chatter is appended to the
session log (or buffered and attached to the next activated run) with no LLM
call. Only an explicit per-chat `always` activation mode runs turns on
unaddressed messages, and then debounced.

**Side effects stay outside the engine.** The `message_send` tool executes as
a worker activity that appends a durable outbox row. The engine sees a normal
tool call and result. Channel connections (grammY, Baileys) stay in the
bridge process; the gateway gains only channel-neutral methods
(`context/append`, `outbox/read`, `outbox/ack`), preserving the P70 rule that
the bridge adds no channel-specific endpoints to Lightspeed.

**Worker never calls the bridge.** Tool execution must not depend on bridge
liveness or reachability. The outbox decouples them: the tool durably
enqueues and returns; the bridge tails, delivers, and acks. Retries,
restarts, and audit fall out of the durable record.

## OpenClaw Reference Study (Channel Side)

The cron/heartbeat side is covered in `p101-timers-schedules-and-triggers.md`.
Channel-side mechanisms worth adopting or adapting
(https://docs.openclaw.ai/channels/groups, /concepts/session,
/concepts/queue, /gateway/heartbeat):

- **Access gating and activation are separate layers.** Allowlists/pairing
  decide who can reach the agent; per-group activation (`mention` default,
  `always`) decides when a turn runs. A runtime `/activation` toggle flips
  modes per chat.
- **Unaddressed group messages are buffered, not dropped** (default 50),
  and injected on the next mention as
  `[Chat messages since your last reply - for context]`. This is what makes
  the bot feel present without replying to everything.
- **Queue discipline:** one active run per session lane; ~500ms debounce
  batches rapid messages; overflow strategies; a "steer" mode injects
  mid-run arrivals into the active turn.
- **Session mapping:** DMs follow a `dmScope` (shared main session vs
  per-peer); groups are always isolated sessions
  (`agent:<id>:telegram:group:<chatId>[:topic:<threadId>]`). Daily/idle
  session resets bound context growth.
- **Outbound is a separate surface:** agents send proactively via message
  tools/actions with channel-specific targets; isolated scheduled runs
  suppress delivery via an explicit decision, with delivery destination
  separate from execution.

Differences for Lightspeed: OpenClaw is a single process, so its in-memory event
queues and direct channel sends are acceptable there; Lightspeed's worker and
bridge are separate processes with a durable store between them, hence the
outbox. OpenClaw's token contracts (`HEARTBEAT_OK`, `NO_REPLY`) are replaced
by delivery policy plus the send tool.

## Current State

- `interop/messaging/src/telegram.ts` / `whatsapp.ts`: thin grammY/Baileys
  adapters. Trigger prefixes are required in groups; non-triggered messages
  are dropped. WhatsApp activation is prefix/`fromMe` checks only — no
  native mention or reply-to-bot detection.
- `interop/messaging/src/runtime.ts`: per-conversation promise queue,
  message dedupe via `.bridge-state.json`.
- `interop/messaging/src/lightspeed.ts`: `session/start` → `run/start`
  (idempotent `submissionId`) → `awaitRun` long-poll → `session/read` →
  extract latest assistant text → reply. One inbound message, one run, one
  outbound message; multi-message runs lose output; nothing can reach the
  chat unless a chat message started a run.
- `crates/api`: `InputItem` is `Text` / `TextRef` only. No context append
  without a run. No outbox. Engine already has
  `CoreAgentCommand::UpsertContext` (not exposed) and blob/CAS methods
  (`blob/put`, `blob/get`).
- Tools execute server-side as Temporal activities; there is no
  client-hosted tool callback mechanism (and this proposal does not add
  one).

## Core Concepts

### Binding

The durable routing record connecting a channel conversation to a session:

```text
Binding {
  channel: "telegram" | "whatsapp",
  account_id: String,
  chat_id: String,
  thread_id: Option<String>,
  session_id: SessionId,
  activation: ActivationPolicy,
  delivery: DeliveryPolicy,
  allow_from: Vec<String>,     // sender allowlist for this chat
  rate_limit: RateLimit,
}
```

Bindings live in the bridge store first (evolving `.bridge-state.json`).
Moving the registry server-side is deliberately deferred (see Non-Goals); the
shape above is written so it can migrate without semantic change.

### Inbound Classification

Every allowed inbound message is classified:

- **UserTurn** — DMs from allowed senders; group messages that natively
  mention the bot, match configured mention names, or reply to a bot
  message. Becomes `run/start` input.
- **RoomEvent** — any other allowed group message. Becomes session context
  via `context/append` (or is buffered and attached to the next UserTurn),
  never an LLM call by itself.
- **Control** — `/activation`, `/new`, `/status`, etc. Handled by the
  bridge; never reaches the session.

### ActivationPolicy

```text
ActivationPolicy = "dm"        // always a UserTurn (direct chats)
                 | "mention"   // groups: mention/reply activates (default)
                 | "always"    // groups: every message is a (debounced) turn
                 | "silent"    // listen only; no turns ever
```

`always` is only sane combined with `message_tool` delivery: the model runs
on (batched) group traffic but is visible only when it explicitly sends.

### DeliveryPolicy

```text
DeliveryPolicy = "automatic"            // final assistant text is delivered
               | "message_tool"         // visible only via messaging tools
               | "silent"               // never delivered (observe/update)
               | "notify_on_completion" // terse completion notice, for
                                        // background/triggered runs
```

Design revision 2026-06-12: `message_tool` is the **target mode for all
bindings, DMs included**, once G5 lands. Chat is not a document: real
participants send several short messages, react, edit, and reply-anchor.
A single final-text blob per run is the wrong shape for the medium, cannot
happen mid-run, and bypasses outbox admission (rate caps, target policy,
audit). Under `message_tool`, final assistant output is **internal** — a run
summary visible to logs/CLI/audit, never delivered to the channel.

`automatic` is demoted to two transitional roles:

- the pre-G5 behavior (what the bridge does today);
- after G5, a per-binding **fallback**: if a run ends with zero messaging
  tool calls in a binding that expects a reply (DM, direct mention), the
  bridge delivers the final text and logs a prompt-contract violation. The
  violation rate is measurable with `crates/eval`; the fallback is dropped
  (or left as config) once it is ~zero.

Interim defaults until G5: DMs and `mention`-mode groups `automatic`;
`always`-mode groups `message_tool`.

### Channel Message Envelope

Group input is useless without attribution. Inbound items carry a structured
envelope rather than bare text: sender id and display name, channel, chat,
thread, timestamp, reply-to message id, and the channel message id.

### OutboundMessage (Outbox)

The single durable delivery spine:

```text
OutboundMessage {
  id: OutboxId,
  origin: ToolCall { session_id, run_id, call_id }
        | FinalText { session_id, run_id }
        | Trigger { trigger_id, firing_id },     // P101, later
  target: Current { session_id }                  // resolved via binding
        | Explicit { channel, account_id, chat_id, thread_id? },
  text: String,
  attachments: Vec<BlobRef>,
  reply_to: Option<String>,
  status: Pending | Delivered { channel_message_id, at }
        | Failed { error, attempts },
  created_at, delivered_at,
}
```

## G1: Bridge Activation Layer And Binding Registry

Rebuild the bridge inbound path around bindings and classification. No Rust
changes required.

Design notes:

- binding registry replaces the ad-hoc conversation map in
  `interop/messaging/src/store.ts`; per-chat activation/delivery/allowlist
  config with global defaults from env/config file;
- DM and group activation per `ActivationPolicy`; Telegram mention detection
  via message entities + bot username; WhatsApp via `mentionedJid` in
  context info and quoted-message author (reply-to-bot), not text prefixes;
- trigger prefixes (`/ask`, `/lightspeed`) remain as an explicit-address fallback
  and for `silent`-mode escape hatches;
- debounce: a quiet window (default 500ms, configurable) batches rapid
  consecutive messages from the same chat into one UserTurn; the existing
  per-conversation queue keeps one active submission per binding;
- control commands handled in the bridge: `/activation mention|always|silent`,
  `/new` (rebind chat to a fresh session id), `/status`; authorization for
  control commands restricted to `allow_from`;
- loop protection: ignore own messages (`fromMe`, bot's own user id),
  ignore messages originating from outbox deliveries (track delivered
  channel message ids), per-chat outbound rate limit with a hard cap;
- Telegram ambient groups require BotFather privacy mode off (document
  this); WhatsApp ambient groups require the sender allowlist to be set.

Acceptance criteria:

- [x] DMs from allowed senders get replies with no trigger prefix;
- [x] in a group in `mention` mode, a native @mention and a reply to a bot
  message both activate; plain chatter does not;
- [x] `/activation` toggles persist across bridge restarts;
- [x] a burst of N quick messages produces one run with all N texts;
- [x] the bridge never reacts to its own deliveries (loop test).

Implemented 2026-06-12: classification and control parsing in
`interop/messaging/src/policy.ts`, debounce/room buffering in `batcher.ts`,
bindings in `store.ts` (legacy conversation state migrates on first touch),
orchestration in `runtime.ts`, native mention/reply detection in the
`telegram.ts` (entities + reply-to) and `whatsapp.ts` (`mentionedJid`,
quoted-author) adapters. Per-chat outbound rate caps are deferred to G4
outbox admission; loop protection today is own-message filtering plus
message dedupe. Covered by vitest suites under `interop/messaging/test/`.

## G2: Room Events Via `context/append`

Expose the engine's existing context-append admission through the gateway so
allowed-but-unaddressed chatter lands in the session log without a run.

Design notes:

- new method `context/append`: `{ sessionId, items: Vec<InputItem> }`,
  admitted as `CoreAgentCommand::UpsertContext` through the same validation
  path as `run/start` input (CAS materialization, size limits), idempotent
  via a client-supplied key like `run/start`'s `submissionId`;
- appended room events are ordinary context entries, rendered with their
  envelope (`[telegram:group Engineering] Alice (12:01): ...`); they are
  data, not instructions — same trust stance as P101 external payloads;
- the bridge batches room events (e.g. flush every 30s or 20 messages,
  whichever first) instead of one RPC per message;
- bounded retention: the bridge stops appending beyond a configurable
  per-binding budget between activations (default ~50 messages, matching
  OpenClaw) and summarizes/drops the overflow — compaction handles the rest;
- until G2 lands, the bridge buffers room events locally and prepends them
  to the next UserTurn input (`[Chat messages since your last reply —
  context]`), which is also the permanent fallback for `session/start`-less
  flows.

Acceptance criteria:

- [x] `context/append` rejects oversized/invalid items at admission (empty
  items, invalid keys, duplicate keys per batch, >64 entries per call);
- [x] room events appear in `session/read` as context items with envelope;
- [x] a subsequent run sees earlier unaddressed chatter (entries are active
  context; verified structurally in the live test);
- [x] duplicate append with the same idempotency key is a no-op.

Implemented 2026-06-12: the entry key is the idempotency handle (no separate
submission id). `crates/engine` now admits `Message { role: User }` entries
through external context edits on non-reserved keys and skips identical
upserts as no-ops (`admit.rs`, `context.rs`); assistant-role edits remain
rejected. The gateway adds `context/append` (`crates/api` method +
`temporal-server` handler with CAS materialization, unchanged-key pre-check,
and `wait_for_context_entries_applied`). Contract artifacts and the TS
client are regenerated. End-to-end coverage in
`crates/temporal-server/tests/temporal_live.rs`
(`temporal_live_context_append_is_idempotent_and_projected`); the bridge
batches room events per chat (flush every 30s / 20 events, budget 50 with
drop reporting) and drains the buffer before an activating turn.

## G3: Structured Input Items And Media Blobs

Extend `api::InputItem` beyond `Text`/`TextRef`.

Design notes:

- `InputItem::ChannelMessage { envelope, content: Vec<InputPart> }` where
  envelope is the structure above and parts are text and/or media refs;
  exact shape to be settled when implementing — the requirement is that
  attribution and timestamps are structured, not string-formatted by the
  bridge;
- the envelope must expose the **channel message id** to the model: G5's
  `message_react`/`message_edit`/`replyTo` need ids to target. Today's text
  envelope omits them (they are hashed into room-entry keys but invisible to
  the model). Cheap interim before G3: include a short id in the envelope
  text (`[telegram:group Eng #4123] Alice: ...`);
- `InputItem::Media { blob_ref, mime, kind: Image | Audio | Document,
  name?, size }`; the bridge downloads media from Telegram/Baileys, uploads
  via `blob/put`, and references it — raw channel payloads stay at the edge,
  blobs stay opaque to the engine per the architecture rules;
- `llm-runtime` adapters materialize image blobs into provider-native image
  parts (Anthropic/OpenAI both accept images natively); documents start as
  provider-native file parts where supported;
- size/type limits enforced at admission (gateway), not in the engine;
- contract artifacts under `interop/contract/` regenerate
  (`cargo run -p api --bin export-schema`) and the TS client picks up the
  new item types.

First cut (2026-06-12): **images in, end to end**; the structured
`ChannelMessage` item is deferred until G5's react/edit tooling needs
machine-readable ids. Scope: `InputItem::Media` accepted for images only
(MIME allowlist, ~10MB cap, per-run count cap; audio/documents get typed
rejections pointing at G6/later); a media item becomes an ordinary
`Message { role: User }` context entry whose `media_type` is the image MIME
and whose blob is the raw bytes — no new engine concept; projections render
such entries from `preview` instead of decoding the blob; `llm-runtime`
adapters emit provider-native image parts (base64) for image-MIME message
entries; the bridge downloads Telegram/WhatsApp photos on user turns and
attaches them (room-event images buffer as placeholder text); envelope text
gains the short channel message id (the G5 prerequisite); media in
`context/append` is rejected for now.

Acceptance criteria:

- [x] sending a photo with a caption in an allowed chat produces a run whose
  input contains a media item + text, and the model describes the image
  (per-provider `#[ignore]` live tests verified against real OpenAI and
  Anthropic APIs 2026-06-12; plumbing covered by unit tests);
- [x] media over the size limit is rejected at admission with a clear error
  (10MB cap via `stat_blob`; bridge logs failed downloads and falls back to
  text);
- [x] contract export includes the new types; stale artifacts fail
  `cargo test -p api`.

First cut implemented 2026-06-12: `InputItem::Media` + `MediaKind` in
`crates/api` (images admitted; audio/document rejected with typed errors);
gateway maps media to `Message { role: User }` entries with image
`media_type` and a `[image: name]` preview (`input.rs`), capped at 8 media
items/run; `context/append` rejects media; `api-projection` renders
non-text message entries from `preview` instead of decoding the blob;
typed image parts added to `llm-clients` (OpenAI `input_image`, Anthropic
`image`/base64 source) and materialized in both `llm-runtime` adapters with
unit tests plus `*_live_adapter_describes_image_input` live tests; the
bridge lazily downloads Telegram photos (getFile) and WhatsApp images
(`downloadMediaMessage`) only for user turns, uploads via `blob/put`, and
attaches media items — room-event images buffer as `(sent an image)`
placeholder text; envelopes now carry the channel message id
(`[telegram:group Eng #4123] ...`).

Second cut (documents) implemented 2026-06-12. Provider support drove the
shape: PDF is the only document type both APIs accept natively (OpenAI
`input_file` data-URL part, Anthropic base64 `document` block); text-based
documents have no native shape on OpenAI and a native `text`-source
`document` block on Anthropic. Accordingly:

- admission accepts `MediaKind::Document` with `application/pdf` (10MB) and
  text MIMEs `text/plain`, `text/markdown`, `text/csv`, `application/json`
  (1MB, validated as UTF-8 at admission); other document types (docx, xlsx,
  ...) get typed rejections;
- a document is a `Message { role: User }` entry with the document MIME and
  a `[document: name]` preview — PDFs are unambiguous by MIME, text
  documents are recognized by the preview marker (ordinary text turns are
  also `text/plain`);
- `llm-runtime` materializes PDFs as provider-native file/document parts
  (filename/title from the preview) and text documents as an Anthropic
  text-source document block / an OpenAI inlined text part with a
  `[document: name]` header; verified by unit tests plus
  `*_live_adapter_reads_pdf_document_input` live tests against both real
  APIs;
- the bridge downloads Telegram `message:document` and WhatsApp
  `documentMessage` attachments on user turns (extension-first MIME
  resolution in `media.ts`, since channels report generic MIMEs for text
  files) and derives the media kind from the MIME instead of hardcoding
  image.

## G4: Delivery Outbox

Durable, channel-neutral outbound delivery through the gateway.

Design notes:

- store-pg table for `OutboundMessage` (plus a store-fs/in-memory variant
  for local mode), keyed by `OutboxId`, with origin, target, payload,
  status, attempt count;
- new methods:
  - `outbox/read { accountFilter?, after: cursor, waitMs }` — long-poll,
    cursor-paginated, same semantics as `session/events/read`;
  - `outbox/ack { outboxId, result: Delivered { channelMessageId } |
    Failed { error, retryable } }`;
- the bridge tails the outbox per account, delivers with channel chunking
  rules (Telegram 4000 chars, WhatsApp 3500 — reuse
  `interop/messaging/src/text.ts`), uploads attachments from CAS, and acks;
- redelivery: unacked entries past a lease timeout become visible again;
  acked-failed-retryable entries retry with backoff up to a cap, then park
  as `Failed` for observability;
- rate limits and loop caps enforced at outbox admission (per session, per
  chat, per window) — a runaway agent is stopped here, not in prompts;
- `automatic` / `notify_on_completion` delivery: in the first cut the
  **bridge** enqueues nothing — it already tails `session/events/read` per
  binding, extracts completed assistant messages, and sends directly. Once
  a server-side binding registry exists (deferred), automatic delivery also
  routes through the outbox and the bridge's session-event path collapses.
  This is an explicit two-step evolution, not an accident.

Acceptance criteria:

- [x] an outbox entry written while the bridge is down is delivered after
  the bridge restarts (pending entries stay visible until acked; the bridge
  cursor restarts at 0);
- [x] at-least-once delivery: the first cut uses a single-consumer
  pending-read + ack model instead of leases — a crash between channel send
  and ack redelivers on restart (residual duplicate window, documented);
  retryable failures re-pend up to 5 attempts, then park as `failed`;
- [x] rate cap rejects enqueues over the limit with a typed tool error
  (per-session 30/minute at tool admission; per-chat granularity arrives
  with explicit targets).

First cut implemented 2026-06-12: new `crates/messaging` (outbox types,
`OutboxStore` trait, in-memory store), `crates/store-pg`
`migrations/005_messaging.sql` + PG impl, gateway `outbox/read` (long-poll,
cursor) and `outbox/ack`, and the bridge `OutboxTailer`
(`interop/messaging/src/outbox.ts`) with per-channel deliverers (Telegram:
send/react/edit via Bot API; WhatsApp: send/react/edit via Baileys).
Verified live against Temporal + Postgres
(`temporal_live_outbox_enqueue_read_ack_round_trip`). Deferred from the G4
spec: lease semantics (single consumer assumed), explicit targets/account
filters on `outbox/read`, attachments on outbound payloads, and
gateway-side automatic delivery (the bridge still owns the final-text
fallback).

## G5: Messaging Tools

The agent's explicit channel vocabulary, and the heart of agent-initiated
messaging. Design revision 2026-06-12: this is a tool *family*, not a single
send tool, and it becomes the only delivery path (see DeliveryPolicy).

The tools:

- `message_send { target, text, replyTo?, attachments? }` — send a message.
  Multiple calls per run are normal (chat-native pacing, mid-run progress
  updates followed by results).
- `message_react { target, messageId, emoji }` — react to a channel message.
  Often the right "ack" for messages that need no text reply.
- `message_edit { target, messageId, text }` — edit a previously sent
  message (own messages only; capability-gated per channel).
- `message_noop { reason? }` — **explicitly decline to reply.** Ends the
  messaging obligation for the turn without delivering anything; the
  optional reason is recorded for audit. This is how "ok"/"thanks" after a
  long exchange ends quietly: deliberate silence is a structured, auditable
  tool call, not an absent send — and not a sentinel token.

Design notes:

- built-in tools in `crates/tools` (alongside host/web packages), executed
  as worker activities. Send/react/edit validate the target, apply outbox
  admission (rate limits, allowed-target policy), append the
  `OutboundMessage`, and return `{ outboxId, status: "enqueued" }` —
  **durable-enqueue semantics, not delivery confirmation**. `message_noop`
  writes no outbox row; the tool call in the session log is its record;
- the **turn contract**: a chat-bound run should end having called at least
  one messaging tool (send, react, edit, or noop). The system-prompt section
  (P65 prompt path) states that final output is never shown to the user and
  is internal notes only. Enforcement is the delivery-policy fallback, not
  provider-level `tool_choice: required` — forcing a tool call every model
  turn breaks loop termination and contaminates non-chat work in the same
  session;
- fallback semantics (transitional): zero messaging-tool calls in a
  reply-expecting binding → bridge delivers final text and logs a contract
  violation. A `message_noop` (or any send/react/edit) suppresses the
  fallback;
- targets: `current` (resolved via the session's binding — requires the
  binding, or a session-config `delivery_target`, to be known server-side;
  first cut: the bridge writes the resolved target into session config at
  binding time), or explicit `{ channel, accountId, chatId, threadId? }`
  restricted by a per-session allowed-targets list;
- `attachments` for outbound media (agent-generated charts, files, images).
  Accepted in two forms: `{ blobRef }` for content already in CAS, and
  `{ workspacePath }` resolved to a blob ref by the tool activity at enqueue
  time (committing to CAS if needed) — the model knows file paths from its
  tools, not hashes. Attachment size/type limits are enforced at outbox
  admission against per-channel caps; the bridge picks the channel encoding
  by MIME (photo vs document) and uses the message text as caption for a
  single attachment. Image *generation* is separate scope: a future
  provider-backed tool that writes to CAS and returns the blob ref composes
  with `message_send` without either knowing about the other;
- `replyTo` for quoting — reply anchoring becomes a model decision,
  replacing the transport-level `replyToMode` heuristics for tool-delivered
  messages;
- react/edit require channel message ids in context — see the G3 envelope
  requirement;
- capability gating per channel: Telegram reactions/edits via Bot API;
  WhatsApp reactions via Baileys; the tool result reports unsupported
  capabilities as typed errors;
- a session without the tools configured simply cannot initiate messages —
  the capability is opt-in per session config.

Acceptance criteria:

- [x] when a run uses messaging tools, final assistant text is not
  delivered — sends arrive via the outbox tail (covered by bridge unit
  tests on the suppression flip);
- [x] a turn answered with `message_noop` delivers nothing and suppresses
  the fallback; a turn with zero successful messaging-tool calls delivers
  the final text as the **default fallback** (not logged as a violation —
  design revision per 2026-06-12 discussion: fallback is normal behavior,
  and the instruction prompting lives in the tool descriptions);
- [ ] the model can react to a specific message and edit one of its own
  messages end-to-end (deliverers implemented for both channels; reactions
  verified in live chat 2026-06-12, the edit smoke test is deliberately
  skipped for now — revisit when edits matter in practice);
- [ ] cross-chat send via explicit target — deliberately deferred; the
  first cut is current-binding only (the bridge resolves session →
  binding, the tools carry no addressing);
- [x] tool result returns immediately even with the bridge offline
  (durable-enqueue semantics; messaging-only batches skip VFS/runtime
  setup entirely).

First cut implemented 2026-06-12: messaging toolset in
`crates/tools/src/messaging/` (four function-tool bundles whose
descriptions carry the turn contract, plus `MessagingToolExecutor` with the
rate cap), enabled per session via `tools.messaging: true` in session
config (`ToolConfigInput`/engine `ToolConfig`/`ToolsetConfig`), executed in
`SessionTools::invoke_batch` (worker) against the PG outbox. The bridge
enables the toolset at `session/start`, detects messaging-tool usage from
run items (`runUsedMessagingTool`), and applies the tools-first flip with
final-text fallback uniformly across bindings — the per-binding
`DeliveryPolicy` enum is deferred until a binding actually needs a
different mode.

## G6: Audio Transcription Preprocessing

Voice notes are first-class input; transcription is a hosted-runtime
concern.

Design notes:

- server-side, not bridge-side: a worker preprocessing activity (alongside
  the `llm-runtime` adapters, which already own provider credentials)
  transcribes audio blobs (OpenAI `whisper-1` / `gpt-4o-transcribe` first)
  and records the transcript as a derived CAS blob linked from the input
  item; the run's planned LLM request sees the transcript text plus an
  `[audio transcript]` marker;
- preprocessing runs between admission and planning as an activity, so the
  engine stays deterministic — it sees the transcript as ordinary input
  facts, and the raw audio stays an opaque blob;
- transcription failures fail the run with a typed error (no silent
  text-less turns); live tests `#[ignore]`d per test rules;
- keeps the bridge thin and credential-free, and makes transcription
  available to every future channel/client, not just this bridge.

Acceptance criteria:

- [ ] a WhatsApp voice note produces a run whose input contains the
  transcript and the model answers the spoken question (live,
  `#[ignore]`);
- [ ] transcription provider errors surface as typed run failures.

## G7: Proactive Runs (Interim Heartbeat, Then Triggers)

Agent-initiated *runs* (as opposed to agent-initiated *messages*, which G4/G5
solve) arrive in two steps:

- **Interim (bridge-side, throwaway):** a per-binding heartbeat timer
  (interval, active-hours window, skip-when-busy) calls `run/start` with a
  small standing-tasks prompt. Output flows through normal delivery policy
  (`notify_on_completion` or `message_tool`), so "nothing to report" runs
  are silent without sentinel tokens. Deliberately minimal — no missed-run
  handling, no durability beyond the binding record.
- **Target (server-side):** the P101 trigger system. Trigger firings start
  runs through the same admission path; `TriggerDelivery` gains an outbox
  variant (`SessionAnnouncement` → enqueue `OutboundMessage` targeting the
  session's binding). The bridge heartbeat is deleted when P101 phases 1–3
  land.

Acceptance criteria:

- [ ] a heartbeat run with nothing to say produces no chat message;
- [ ] a heartbeat run with a finding delivers it to the bound chat;
- [ ] heartbeat skips outside active hours and while a run is active.

## Non-Goals

- No channel connections (grammY/Baileys/webhooks) inside Lightspeed crates; the
  bridge process owns transports. The gateway gains only channel-neutral
  methods.
- No universal chat message model in the engine; envelopes and media are
  `api`-level input shapes, payloads beyond reducer facts stay opaque/blob.
- No mirroring of chats between channels; bindings attach channels to
  sessions, sessions do not bridge channels to each other.
- No client-hosted tool callback mechanism; the worker never calls the
  bridge.
- No SSE/WebSocket push; long-poll cursors (P70) carry both session events
  and the outbox for now.
- No server-side binding registry yet; bridge-local bindings are sufficient
  until a second consumer of the mapping exists (gateway-side automatic
  delivery, fleet tooling). Revisit at G4's second step.
- No sentinel-token silence contracts.

## Safety And Trust

- Allowlists default-on for real use (current empty-allowlist warning gets a
  config flag to hard-fail in non-dev mode). Deferred 2026-06-12: a proper
  login/authorization system is planned next and supersedes the flag.
- Inbound channel text and media are untrusted data — same stance as P101
  external trigger payloads; envelopes make provenance explicit to the
  model.
- Outbox admission is the enforcement point: per-chat/per-session rate
  caps, allowed-target lists, attachment size limits.
- Loop protection at the bridge (own-message and delivery-echo filtering)
  *and* at outbox admission (rate caps) — neither alone is sufficient.
- Control commands restricted to `allow_from`; pairing flows for unknown DM
  senders are future work.

## Open Questions

- **dmScope:** current per-chat sessions match OpenClaw's
  `per-channel-peer`. A shared "main" session across the owner's DMs on all
  channels is what makes a personal assistant feel continuous — worth a
  binding-level option once session lifecycle exists.
- **Session lifecycle:** permanent per-chat sessions grow without bound and
  lean entirely on compaction. Daily/idle reset with `/new` exists in
  OpenClaw; Lightspeed needs a position (likely bridge-side rebinding first,
  engine-side session forking later).
- **Mid-run steering:** the engine has `RequestRunSteering` but the gateway
  does not expose it. Without it, messages arriving mid-run queue as
  follow-up turns. Exposing `run/steer` would enable OpenClaw-style steer
  mode; deferred until the queueing UX proves insufficient.
- **Read receipts / typing indicators:** typing indicators shipped
  2026-06-12 — the bridge fires the channel typing action (Telegram
  `sendChatAction`, WhatsApp `composing` presence) when a turn starts
  processing and refreshes it until the run completes (capped at 3
  minutes). Read receipts remain open.
- **Group sender pairing:** per-sender authorization inside allowed groups
  (OpenClaw's `groupAllowFrom`) vs chat-level allowlists only.

## Phasing

1. **G1** — bridge activation layer, bindings, debounce, loop protection
   (TypeScript only; immediately useful).
2. **G2 + G3** — `context/append`, envelope/media input items, image
   support through `llm-runtime`, contract regeneration.
3. **G4 + G5** — outbox + messaging tool family (send/react/edit/noop) +
   delivery policies; ambient group mode and agent-initiated messages become
   real. Then flip DMs to `message_tool` with the zero-tool-call fallback,
   measure contract violations, and retire `automatic`.
4. **G7 interim** — bridge heartbeat (small, after G4/G5 so delivery policy
   applies).
5. **G6** — audio transcription activity.
6. **P101 phases 1–3** supersede the interim heartbeat; trigger delivery
   targets the outbox.
