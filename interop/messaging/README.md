# Forge Messaging Bridge

Telegram and WhatsApp bridge process for Forge sessions.

This is an overlay service: it does not add channel-specific endpoints to the
Forge gateway. It normalizes chat messages, maps each chat/thread to a stable
Forge session, submits text through the local `@forge/agent-client` package, and
sends the latest assistant message back to the channel.

## Shape

- `src/runtime.ts` owns dedupe, per-conversation serialization, and retry state.
- `src/forge.ts` owns `session/start`, `run/start`, `awaitRun`, and reply
  extraction.
- `src/telegram.ts` is a thin grammY adapter.
- `src/whatsapp.ts` is a thin Baileys adapter for a WhatsApp Web spare account.
- `.bridge-state.json` stores chat-to-session mappings, event cursors, and
  message dedupe records.

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
TELEGRAM_TRIGGER_PREFIXES=/ask,/forge
TELEGRAM_REQUIRE_TRIGGER=true
```

Use `/ask question` or `/forge question` in the allowed chats. In groups,
triggers are always required.

## WhatsApp

Use a spare WhatsApp number. Configure:

```bash
WHATSAPP_ENABLED=true
WHATSAPP_AUTH_DIR=.whatsapp-auth
WHATSAPP_ALLOWED_JIDS=41790000000@s.whatsapp.net,120363000000000000@g.us
WHATSAPP_TRIGGER_PREFIXES=/ask,/forge
WHATSAPP_REQUIRE_TRIGGER=true
WHATSAPP_PRINT_QR=true
```

On first run the bridge prints a QR code. Link it from WhatsApp on the spare
phone/account. For groups, use the group JID and keep triggers enabled.

## Safety Defaults

Set allowlists for real use. If `TELEGRAM_ALLOWED_CHAT_IDS` or
`WHATSAPP_ALLOWED_JIDS` is empty, the bridge logs a warning and accepts any chat
that can reach the bot/account.

The bridge handles text, image captions, video captions, and document captions
on WhatsApp. It does not download media yet.

## Verify

```bash
npm run check
```
