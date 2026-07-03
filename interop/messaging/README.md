# Lightspeed Messaging Bridge

Telegram and WhatsApp channel gateway for Lightspeed sessions (P71 G1/G2).

This is an overlay service: it does not add channel-specific endpoints to the
Lightspeed gateway. It binds each chat/thread to a stable Lightspeed session, classifies
inbound traffic, submits addressed messages as runs, appends unaddressed group
chatter as session context, and sends assistant replies back to the channel.

Access is gated by **sender handle** or **pairing code**, and conversations bind
to **agent profiles** that configure model, tools, instructions, mounted
workspaces/snapshots, linked MCP servers, and attached execution environments.
See [Access control and bindings](#access-control-and-bindings).

## How a message flows

Every allowed inbound message is classified, then admitted through one of the
Lightspeed input paths:

- **User turn** — direct messages, `always` group messages, and explicitly
  triggered messages start a run. Direct and `always` turns use
  `run/start source=input`, which atomically ingests text/media and starts
  work. Mention-mode group turns append the inbound message first and start
  with `run/start source=context`, so the run is triggered by the committed
  context keys instead of duplicate input.
- **Room event** — any other allowed group message. Ingest is eager: every
  allowed group message — text-only or media-bearing — is downloaded as needed
  and appended as session context through `context/append` at receipt, before
  activation is evaluated, so audio transcripts can participate in activation
  and room history survives bridge restarts. Transcript activation is matched
  only against server-derived media entries; plain text was already
  mention-checked at classification. No LLM call is started unless activation
  matches after append.
- **Control** — `/activation mention|always|silent`, `/status`. Restricted to
  `*_CONTROL_ALLOW_FROM` senders (with the list empty, direct chats are
  trusted; group members are not).
- **Pairing required** — the conversation matches a pairable binding but has
  not sent the configured code yet. No message reaches Lightspeed until the
  chat pairs.
- **Denied** — the sender is not on the channel's turn allowlist
  (`*_ALLOW_FROM`) and is not covered by a paired binding. Direct chats get one
  authorization-error reply; group members are dropped silently. With the
  allowlist empty, anyone may chat unless a pairable binding requires pairing.

Replies anchor contextually: in groups the reply quotes the first message of
the answered batch (the question), and only on the first chunk of a long
reply; direct chats never quote. Configure per channel with
`*_REPLY_TO_MODE` (`off` | `first` | `all`, default `first`).

## Messaging tools and the outbox (P71 G4/G5)

Sessions the bridge creates get the messaging toolset
(`message_send`, `message_react`, `message_edit`, `message_noop`). The model
speaks by calling tools: sends/reactions/edits land as durable outbox rows
on the gateway, and the bridge tails `outbox/read`, resolves each entry's
session to its chat binding, delivers through the channel API, and acks.
This also means a Lightspeed session can message the chat mid-run or from a run
the user never started.

When a run uses **no** messaging tool, the bridge falls back to delivering
the final assistant text — that is the default for plain Q&A, not an error.
`message_noop` is the explicit "no reply needed" so a closing "thanks" ends
quietly. Delivery is at-least-once: a bridge crash between channel send and
ack redelivers on restart; retryable failures are retried up to 5 attempts,
then parked for inspection. Outbound sends are rate-capped per session
(30/minute) at tool admission.

Per-chat activation is persisted in `.bridge-state.json` and toggled at
runtime with `/activation`:

- `mention` (group default) — speak only when addressed; listen otherwise.
- `always` — every group message becomes a turn (use with care; pair with
  P71 G5 delivery policies once those exist).
- `silent` — listen only; trigger prefixes remain as the escape hatch.

Activation is evaluated against native adapter facts (mentions and replies),
raw message text/captions, and `activationText` returned by `context/append`.
For audio messages, `activationText` is the transcript text, matched against
the configured `triggerPrefixes`, `mentionNames`, and the bot's username — so
a voice note that says a trigger prefix or mention name can start a run in
mention mode after transcription succeeds. Direct messages are always active
and skip the transcript-matching step. A group voice note whose transcription
fails never starts a run.

## Room context retention

Eager ingest makes session context grow per room message, so the bridge
bounds each conversation's **unconsumed backlog**: room entries appended
since the last run the bridge started there. When the backlog exceeds
`BRIDGE_ROOM_RETENTION_HIGH` messages, the oldest messages are removed via
`context/remove` down to `BRIDGE_ROOM_RETENTION_LOW`, oldest first and only
while the conversation is idle (no queued or in-flight turn, and never a
queued turn's trigger keys). The watermark gap keeps the room span
append-only between prunes, so provider prompt caches stay warm. Entries any
run has already seen are consumed history and are never pruned — their
lifecycle belongs to server-side compaction. The backlog list is in-memory;
a bridge restart starts it empty, which only delays pruning.

| Variable | Default | Purpose |
|---|---|---|
| `BRIDGE_ROOM_RETENTION_HIGH` | `300` | Unconsumed backlog size (in messages) that triggers a prune; `0` disables retention |
| `BRIDGE_ROOM_RETENTION_LOW` | `200` | Backlog size a prune reduces to (must be smaller than `HIGH`) |

## Access control and bindings

Two things are configured per conversation: **who may use the bot** and **what
session a conversation binds to**.

### Sender allowlist (security)

Each channel has a turn allowlist (`*_ALLOW_FROM`, env CSV or the config file):

- **Empty** — anyone in an allowed chat may chat (the open default).
- **Set** — only listed handles may take a turn. An unlisted *direct* sender
  gets one authorization-error reply; an unlisted *group* member is dropped
  silently (no per-message spam). Handles match case-insensitively and ignore a
  leading `@`, and either identity works (a Telegram numeric id or `@username`;
  a WhatsApp phone JID or bare number).

A separate `*_CONTROL_ALLOW_FROM` gates control commands; empty trusts direct
chats only.

### Pairing codes (security)

Bindings may require chat-level pairing instead of maintaining sender handles:

```jsonc
{
  "id": "personal-whatsapp",
  "match": { "channel": "whatsapp", "scope": "direct" },
  "profile": "personal",
  "sessionKey": "lukas",
  "pairing": { "codeEnv": "PERSONAL_PAIRING_CODE" }
}
```

Use `"pairing": { "code": "hardcoded-code" }` for local/private deployments, or
`"codeEnv"` to load the code from an environment variable. Pairable bindings
must have a stable `id`; the bridge persists that id after a successful pairing.

Until paired, direct chats receive: `This chat is not paired yet. Send the
pairing code to connect it.` Group chats are quieter: ambient messages are
dropped, and the bridge only prompts when the bot is addressed or a trigger
prefix is used. Sending the exact code pairs the chat and returns `Paired. You
can now message Lightspeed from this chat.`

Group pairing is by chat, not by member. Once a group is paired to a binding,
any member of that group can use the bound profile/session.

### Profiles and bindings (configuration)

The bridge does not provision skills or system prompts directly. Instead each
binding can reference a first-class Lightspeed **agent profile**. A profile points
a session at a model, a tool set, instructions, mounted VFS workspaces/snapshots,
linked MCP servers, and optional execution environments. Profiles live in the
Lightspeed profile registry (`lightspeed profiles create ...`) or can be supplied
inline in a binding. A complete, runnable example is in
[`bridge.config.example.json`](bridge.config.example.json) — copy it to
`bridge.config.json` and point `BRIDGE_CONFIG` at it:

```jsonc
{
  "telegram": { "allowFrom": [], "controlAllowFrom": ["@lukas"] },

  "bindings": [
    { "id": "personal-chat", "match": { "channel": ["telegram", "whatsapp"] }, "profile": "personal", "sessionKey": "lukas", "pairing": { "codeEnv": "PERSONAL_PAIRING_CODE" } },
    {
      "id": "eng-room",
      "match": { "channel": "telegram", "chatId": "-100123", "scope": "group" },
      "profile": {
        "kind": "inline",
        "profile": {
          "config": { "tools": { "messaging": true, "webSearch": true } },
          "mounts": [
            { "mountPath": "/playbook", "source": { "type": "snapshot", "snapshotRef": "snap_support_v3" }, "access": "readOnly" }
          ]
        }
      },
      "sessionKey": "eng-room",
      "pairing": { "code": "demo-room-code" }
    },
    { "match": { "channel": "*" } }
  ]
}
```

- **Bindings** are evaluated top-to-bottom, first match wins. Consecutive
  matching pairable bindings may share the same broad match; before a chat is
  paired, the code selects which binding id to persist. `match` filters by
  `channel` (`telegram` | `whatsapp` | `*`, or an array such as
  `["telegram", "whatsapp"]`), and optional `handle`, `chatId`, and `scope`
  (`direct` | `group`). `handle` may be one string or an array of aliases for
  the same sender.
- **`profile`** is optional. A string is shorthand for a named registry profile.
  An object is passed as a `ProfileSource`, either `{ "kind": "named",
  "profileId": "support" }` or `{ "kind": "inline", "profile": { ... } }`.
- **`sessionKey`** ties conversations to a session within one channel/account.
  Conversations sharing a key on the same provider account share one session
  (e.g. a team and its members); omit it and each conversation gets its own.
  The provider/account remain part of the derived session id, so a combined
  Telegram+WhatsApp binding shares profile and pairing policy, not one
  cross-channel transcript. There is no `/new` — a conversation always resolves
  to the session for its key.
- **Profile documents** reuse the API profile shape: `config`, `instructions`,
  `mounts`, `mcp`, and `environments`. The bridge passes them to
  `session/start { profile }`; the hosted profile applier handles all
  mount/link/attach work.
- Legacy `recipes` and `bindings[].recipe` are not accepted. Move reusable setup
  into the profile registry, or wrap the same document inline under
  `bindings[].profile`.

A conversation with no matching binding (or a matching binding with no profile)
gets the default: a per-conversation session id, no mounts, no MCP, no
environments, messaging tool only.

## Shape

- `src/config.ts` — env + JSON config loading, profile/binding parsing, and
  per-inbound access resolution (allowlists + binding match).
- `src/policy.ts` — inbound classification (incl. the `denied` outcome),
  control-command parsing, and the message envelope
  (`[telegram:group Engineering] Alice (12:01Z): ...`).
- `src/batcher.ts` — turn debouncing.
- `src/runtime.ts` — orchestration: bindings, dedupe, per-conversation
  serialization, denied handling, control commands.
- `src/lightspeed.ts` — `session/start { profile }`, `context/append`,
  `context/remove`, `run/start`, awaitRun, and reply extraction.
- `src/store.ts` — pairings, bindings (chat → session, profile label,
  activation, cursor), plus message dedupe records in `.bridge-state.json`.
- `src/telegram.ts` — grammY adapter with native mention/reply detection.
- `src/whatsapp.ts` — Baileys adapter with `mentionedJid`/quoted-author
  detection for a WhatsApp Web spare account.

## Setup

Start the Lightspeed gateway first:

```bash
cargo run -p temporal-server
```

Install and run the bridge:

```bash
cd interop/messaging
npm install
cp .env.example .env
npm run dev
```

### Gateway authentication

Against a default (`single`-mode) gateway no credentials are needed. For
multi-tenant gateways (P90), set one of:

- `LIGHTSPEED_API_KEY` (or `lightspeed.apiKey` in the config file) — sent as
  `Authorization: Bearer …` to an `api-key`-mode gateway. Mint keys with
  `server api-key create --universe-id <uuid>`.
- `LIGHTSPEED_UNIVERSE` (or `lightspeed.universe`) — sent as
  `x-lightspeed-universe` to a `trusted-header`-mode gateway (the bridge acts
  as its own trusted upstream in that topology).

The credential selects the universe all of this bridge's sessions live in:
one bridge process serves one universe. To bridge several universes, run one
bridge process per universe. Per-binding credentials (universes mixed within
one process) are a recorded follow-up in P90.

The package uses a local file dependency:

```json
"@lightspeed/agent-client": "file:../ts-client"
```

Bridge scripts run `npm --prefix ../ts-client run build` first, so
the local client package export is available even though `dist/` is ignored.

## Telegram

Create a bot with BotFather, then configure:

```bash
TELEGRAM_BOT_TOKEN=123:abc
TELEGRAM_ALLOWED_CHAT_IDS=-1001234567890,123456789
TELEGRAM_ALLOW_FROM=123456789,@lukas
TELEGRAM_CONTROL_ALLOW_FROM=@lukas
TELEGRAM_GROUP_ACTIVATION=mention
```

DMs from allowed senders answer without any trigger. In groups, @mention the
bot or reply to one of its messages. `TELEGRAM_ALLOW_FROM` accepts numeric ids
or `@usernames`; leave it empty to let anyone in an allowed chat chat.

For the bot to see unaddressed group messages (room context, `always` mode),
disable privacy mode in BotFather (`/setprivacy` → Disable). With privacy
mode on, Telegram only delivers commands and replies to the bot.

## WhatsApp

Use a spare WhatsApp number. Configure:

```bash
WHATSAPP_ENABLED=true
WHATSAPP_AUTH_DIR=.whatsapp-auth
WHATSAPP_ALLOWED_JIDS=41790000000@s.whatsapp.net,120363000000000000@g.us
WHATSAPP_ALLOW_FROM=41790000000@s.whatsapp.net
WHATSAPP_CONTROL_ALLOW_FROM=41790000000@s.whatsapp.net
WHATSAPP_GROUP_ACTIVATION=mention
WHATSAPP_PRINT_QR=true
```

On first run the bridge prints a QR code. Link it from WhatsApp on the spare
phone/account. Group activation uses native @mentions (`mentionedJid`) and
replies to the bot's messages.

## Safety Defaults

Set allowlists for real use. Two layers gate inbound traffic:

- **Chat allowlist** (`TELEGRAM_ALLOWED_CHAT_IDS` / `WHATSAPP_ALLOWED_JIDS`) —
  which chats the adapter listens to at all. Empty logs a warning and accepts
  any chat that can reach the bot/account.
- **Sender allowlist** (`*_ALLOW_FROM`) — which handles may take a turn within
  an allowed chat. Empty logs a warning and lets anyone chat; set it to lock
  the bot to specific people. See
  [Access control and bindings](#access-control-and-bindings).

Loop protection: the bridge ignores its own messages (`fromMe`, bot user id)
and deduplicates inbound deliveries; room-event appends are idempotent per
message key, so channel redelivery is harmless.

The bridge handles text, images, documents, and audio using the same media
boundaries as the Lightspeed API. For allowed non-control messages, current
supported media is downloaded, stored in CAS via `blob/put`, and admitted as
either context append entries or atomic run input:

- **Images** are attached as model-visible image context/input. They do not
  produce activation text by themselves.
- **Documents** include PDF up to 10MB and markdown, plain text, CSV, and JSON
  up to 1MB. They are model-visible document context/input, but document body
  text is not treated as a wake command.
- **Audio** is transcribed by the hosted preprocessing path. Context append
  stores the transcript as text context and returns transcript activation text.
  If transcription or transcoding fails, the append result is logged with a
  typed failure and the bridge does not submit an empty context-triggered run.
- **Video and unsupported document types** remain caption-only/unsupported;
  the bridge does not upload raw video media.

Ingest failures are split by kind. Transient append failures (network errors,
`admissionRejected` results) leave the message marked retryable, so channel
redelivery retries the append instead of dropping the message. Terminal ingest
failures on an addressed mention-mode turn get an error reply in the chat
(`Lightspeed could not ingest this message: ...`); audio failures on direct
and `always` turns reply `Lightspeed could not transcribe this audio
message: ...`; unaddressed group messages are logged only. Group voice notes
that fail transcription do not start runs in any mode.

While a turn is running the bridge shows a typing indicator in the chat
(Telegram `sendChatAction`, WhatsApp `composing` presence), refreshed until
the run completes.

Outbound text is markdown from the model; the bridge renders it per channel
(`src/markdown.ts`): Telegram gets the `parse_mode=HTML` subset with a
plain-text fallback if Telegram rejects the entities, WhatsApp gets its
native inline syntax (`*bold*`, `_italic_`). Headings, lists, and tables
downconvert to plain text shapes.

## Verify

```bash
npm run check
```
