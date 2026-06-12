# Forge Messaging Bridge

Telegram and WhatsApp channel gateway for Forge sessions (P71 G1/G2).

This is an overlay service: it does not add channel-specific endpoints to the
Forge gateway. It binds each chat/thread to a stable Forge session, classifies
inbound traffic, submits addressed messages as runs, appends unaddressed group
chatter as session context, and sends assistant replies back to the channel.

## How a message flows

Every allowed inbound message is classified:

- **User turn** — direct messages, group messages that @mention the bot,
  reply to a bot message, or start with a trigger prefix. Rapid consecutive
  messages are debounced (default 500ms quiet window) into one run.
- **Room event** — any other group message. It is buffered and appended to
  the bound session via `context/append` (batched, idempotent per message),
  so the next activated turn already knows the conversation. No LLM call.
- **Control** — `/activation mention|always|silent`, `/new` (fresh session
  for the chat), `/status`. Restricted to `*_ALLOW_FROM` senders (with the
  list empty, direct chats are trusted; group members are not).

Replies anchor contextually: in groups the reply quotes the first message of
the answered batch (the question), and only on the first chunk of a long
reply; direct chats never quote. Configure per channel with
`*_REPLY_TO_MODE` (`off` | `first` | `all`, default `first`).

Per-chat activation is persisted in `.bridge-state.json` and toggled at
runtime with `/activation`:

- `mention` (group default) — speak only when addressed; listen otherwise.
- `always` — every group message becomes a turn (use with care; pair with
  P71 G5 delivery policies once those exist).
- `silent` — listen only; trigger prefixes remain as the escape hatch.

## Shape

- `src/policy.ts` — inbound classification, control-command parsing, and the
  message envelope (`[telegram:group Engineering] Alice (12:01Z): ...`).
- `src/batcher.ts` — turn debouncing and room-event buffering/budgets.
- `src/runtime.ts` — orchestration: bindings, dedupe, per-conversation
  serialization, control commands.
- `src/forge.ts` — `session/start`, `context/append`, `run/start`, awaitRun,
  and reply extraction (all assistant messages of a run).
- `src/store.ts` — bindings (chat → session, activation, cursor) plus message
  dedupe records in `.bridge-state.json`.
- `src/telegram.ts` — grammY adapter with native mention/reply detection.
- `src/whatsapp.ts` — Baileys adapter with `mentionedJid`/quoted-author
  detection for a WhatsApp Web spare account.

## Setup

Start the Forge gateway first:

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
"@forge/agent-client": "file:../ts-client"
```

Bridge scripts run `npm --prefix ../ts-client run build` first, so
the local client package export is available even though `dist/` is ignored.

## Telegram

Create a bot with BotFather, then configure:

```bash
TELEGRAM_BOT_TOKEN=123:abc
TELEGRAM_ALLOWED_CHAT_IDS=-1001234567890,123456789
TELEGRAM_ALLOW_FROM=123456789
TELEGRAM_GROUP_ACTIVATION=mention
```

DMs from allowed chats answer without any trigger. In groups, @mention the
bot or reply to one of its messages.

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
WHATSAPP_GROUP_ACTIVATION=mention
WHATSAPP_PRINT_QR=true
```

On first run the bridge prints a QR code. Link it from WhatsApp on the spare
phone/account. Group activation uses native @mentions (`mentionedJid`) and
replies to the bot's messages.

## Safety Defaults

Set allowlists for real use. If `TELEGRAM_ALLOWED_CHAT_IDS` or
`WHATSAPP_ALLOWED_JIDS` is empty, the bridge logs a warning and accepts any
chat that can reach the bot/account.

Loop protection: the bridge ignores its own messages (`fromMe`, bot user id)
and deduplicates inbound deliveries; room-event appends are idempotent per
message key, so channel redelivery is harmless.

The bridge handles text and images on both channels (P71 G3 first cut):
photos in user turns are downloaded, stored in CAS via `blob/put`, and
attached to the run as image input the model sees natively. Images are only
downloaded for messages that activate a turn — unaddressed group photos
buffer as `(sent an image)` placeholder text. Video/document/voice media is
caption-only for now (audio transcription is P71 G6).

## Verify

```bash
npm run check
```
