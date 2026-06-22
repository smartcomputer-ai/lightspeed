# Lightspeed Messaging Bridge

Telegram and WhatsApp channel gateway for Lightspeed sessions (P71 G1/G2).

This is an overlay service: it does not add channel-specific endpoints to the
Lightspeed gateway. It binds each chat/thread to a stable Lightspeed session, classifies
inbound traffic, submits addressed messages as runs, appends unaddressed group
chatter as session context, and sends assistant replies back to the channel.

Access is gated by **sender handle**, and conversations bind to **session
recipes** that configure model, tools, mounted workspaces/snapshots, linked MCP
servers, and attached execution environments. See
[Access control and bindings](#access-control-and-bindings).

## How a message flows

Every allowed inbound message is classified:

- **User turn** — direct messages, group messages that @mention the bot,
  reply to a bot message, or start with a trigger prefix. Rapid consecutive
  messages are debounced (default 500ms quiet window) into one run.
- **Room event** — any other group message. It is buffered and appended to
  the bound session via `context/append` (batched, idempotent per message),
  so the next activated turn already knows the conversation. No LLM call.
- **Control** — `/activation mention|always|silent`, `/status`. Restricted to
  `*_CONTROL_ALLOW_FROM` senders (with the list empty, direct chats are
  trusted; group members are not).
- **Denied** — the sender is not on the channel's turn allowlist
  (`*_ALLOW_FROM`). Direct chats get one authorization-error reply; group
  members are dropped silently. With the allowlist empty, anyone may chat.

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

### Recipes and bindings (configuration)

The bridge does not provision skills or system prompts directly. Instead a
**recipe** points a session at a model, a tool set, mounted VFS
workspaces/snapshots, linked MCP servers, and optional execution environments.
The core then discovers `.lightspeed/prompts/` instructions and the skill
catalog *from the mounts*, and the agent activates skills itself. Recipes and
bindings live in the JSON file at `BRIDGE_CONFIG`. A complete, runnable example is in
[`bridge.config.example.json`](bridge.config.example.json) — copy it to
`bridge.config.json` and point `BRIDGE_CONFIG` at it:

```jsonc
{
  "telegram": { "allowFrom": ["@lukas"], "controlAllowFrom": ["@lukas"] },

  "recipes": {
    "personal": {
      "config": {
        "model": { "providerId": "anthropic", "apiKind": "anthropic_messages", "model": "..." },
        "tools": { "messaging": true, "filesystem": "edit", "webSearch": true }
      },
      "mounts": [
        { "mountPath": "/workspace", "source": { "workspaceId": "lukas-ws" }, "access": "readWrite" }
      ],
      "mcp": [
        { "serverId": "github-mcp", "allowedTools": ["search_issues"], "approval": "never" }
      ],
      "environments": [
        { "envId": "devbox", "providerId": "hetzner-devbox", "targetId": "local", "activate": true }
      ]
    }
  },

  "bindings": [
    { "match": { "channel": "telegram", "handle": ["@lukas", "6071843755"] }, "recipe": "personal", "sessionKey": "lukas" },
    { "match": { "channel": "telegram", "chatId": "-100123", "scope": "group" }, "recipe": "personal", "sessionKey": "eng-room" },
    { "match": { "channel": "*" } }
  ]
}
```

- **Bindings** are evaluated top-to-bottom, first match wins. `match` filters by
  `channel` (`telegram` | `whatsapp` | `*`), and optional `handle`, `chatId`,
  and `scope` (`direct` | `group`). `handle` may be one string or an array of
  aliases for the same sender.
- **`sessionKey`** ties conversations to a session. Conversations sharing a key
  share one session (e.g. a team and its members); omit it and each
  conversation gets its own. There is no `/new` — a conversation always resolves
  to the session for its key.
- **`config`** is passed straight to `session/start` (model, tools,
  generation…); the messaging toolset defaults on unless a recipe disables it.
- **`mounts`** default `mountPath` to `/workspace` and `access` to `readWrite`;
  `source` is `{ workspaceId }` or `{ snapshotRef }`. Create the workspace or
  snapshot out of band (`vfs/workspace/create`, `vfs/snapshot/commit`).
- **`mcp`** is the `session/mcp/link` surface; the server must already be created
  and authenticated (`mcp/servers/create`). The recipe references it by id.
- **`environments`** attaches existing provider targets to the session through
  `session/environments/attach`. The provider must already be online. `envId`
  and `providerId` are required, `targetId` defaults to `local`, and `activate`
  defaults to `true`. At most one environment may have `activate: true`; `envs`
  is accepted as a short alias.

A conversation with no matching binding (or a matching binding with no recipe)
gets the default: a per-conversation session id, no mounts, no MCP, no
environments, messaging tool only.

## Shape

- `src/config.ts` — env + JSON config loading, recipe/binding parsing, and
  per-inbound access resolution (allowlists + binding match).
- `src/policy.ts` — inbound classification (incl. the `denied` outcome),
  control-command parsing, and the message envelope
  (`[telegram:group Engineering] Alice (12:01Z): ...`).
- `src/batcher.ts` — turn debouncing and room-event buffering/budgets.
- `src/runtime.ts` — orchestration: bindings, dedupe, per-conversation
  serialization, denied handling, control commands.
- `src/lightspeed.ts` — `session/start` + recipe provisioning (mounts, MCP
  links, environments), `context/append`, `run/start`, awaitRun, and reply
  extraction.
- `src/store.ts` — bindings (chat → session, recipe, activation, cursor) plus
  message dedupe records in `.bridge-state.json`.
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

The bridge handles text, images, and documents on both channels (P71 G3):
photos and supported document attachments (PDF up to 10MB; markdown, plain
text, CSV, and JSON up to 1MB) in user turns are downloaded, stored in CAS
via `blob/put`, and attached to the run as input the model sees natively.
Media is only downloaded for messages that activate a turn — unaddressed
group attachments buffer as `(sent an image)` / `(sent a file: ...)`
placeholder text. Other document types and video are caption-only; voice
notes await audio transcription (P71 G6).

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
