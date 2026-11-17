# Chat channels

Chat channels let people talk to Lightspeed bots through Telegram or WhatsApp.
A connected account supplies the provider connection. A bot's chat trigger
decides which conversations it accepts, how people gain access, and when a
message should wake the agent.

Connecting the account is therefore only the first step. You also bind a
provider conversation to a bot trigger. With the default routing, each paired
conversation keeps its own agent session and history. Custom routing can
deliberately combine conversations.

Start with a working bot from [Bots and triggers](bots-and-triggers.md) and a
model suited to its tasks. Use Operator or Owner access to manage channel
accounts and bot triggers. A universe Owner enables **Bots** and **Channels**
under **Settings → General → Features**. The channel controls then appear
under **Settings → Channels**.

## Make the connector available

Ask your deployment operator to start a connector host with the provider
you plan to use. It runs separately from the development launcher and
connects to the private runtime API and Temporal. See
[Channel connectors](../integrating-and-extending/channel-connectors.md)
for setup and health checks.

## Connect a Telegram account

Open **Settings → Channels → Connect channel** and choose **Telegram**. Enter the bot
token supplied by BotFather, optionally set a display name, and choose
**Connect Telegram**.

Lightspeed checks the token against Telegram and encrypts it for the account.
The connector discovers the account and starts serving it.
Allow for its discovery interval, which defaults to 30 seconds.

Use only one Telegram update consumer for this token. A second connector host
or another application polling the same bot can interfere with receiving
messages.

## Connect a WhatsApp account

Under **Channels → Connect channel**, choose **WhatsApp**, enter the account's
**Phone number**, leave **Print pairing QR code** selected, and choose
**Connect WhatsApp**.

Read the QR code in the connector's process output and scan it through the
WhatsApp account's linked-device flow. The connector stores the linked-device
authentication in its persistent directory.

This QR pairing authorizes Lightspeed to use the WhatsApp account. The
Lightspeed conversation-pairing code in the next step determines which bot
handles a particular chat. They are separate operations.

## Pair a conversation with a bot

For a first test, use a direct chat and retain code-based pairing:

1. Open the target bot and choose **Settings → Triggers → Add trigger → Chat
   account**.
2. Select the connected **Messaging account**.
3. Set **Conversations** to **Direct chats only**.
4. Keep **Require a pairing code** enabled and the default **One session per
   conversation** routing. Add the trigger.
5. Copy the pairing code from the saved trigger. It is shown to Operators
   and Owners only.
6. From another account, open a direct chat with the Telegram bot or WhatsApp
   number. Send the pairing code as the entire message.

The connector should reply that the chat is paired and ready for messages.
Now send a task, for example:

```text
Review the current Acorn release notes against the saved change list.
Tell me whether any claims need correction. Leave the files unchanged.
```

Inspect the bot's provider-labeled conversation and the `chat.message` event
in Activity. Check **Channels → Connected conversations** for the binding.
The response in the provider chat and the corresponding Lightspeed activity
verify the complete path from account connection through agent execution to
delivery.

The trigger opens a session for the paired chat. Forum threads get separate
sessions under the same binding, and incoming work queues when the session
is busy.

## Set access and group activation

Pairing controls which chat belongs to the trigger. To restrict individual
senders, set **Who may talk to the bot → Listed handles only**, then enter
**Allowed handles**. Enter Telegram numeric user IDs or WhatsApp JIDs. The
field expects those provider identifiers, even if the placeholder suggests
a display name or `@username`.

**Control commands** is a separate list of senders allowed to use `/status`
and `/activation mention` or `/activation always`. An empty control list
grants those commands to nobody. Being allowed to send ordinary messages does
not by itself grant control commands.

Authorized direct messages activate the bot. For groups, the default
activation rule responds to mentions, replies, or configured prefixes such as
`/ask` and `/lightspeed`. Use always-on group activation only when the bot
should handle every otherwise eligible message. Ambient messages that fail
activation are dropped; they are not quietly saved as extra agent context.

An account can serve several chat triggers. For an unpaired conversation,
matching open triggers are considered first, with lower priority numbers
winning. Code-based triggers are considered only if no open trigger matches.
An open trigger can therefore accept a chat before a code-based trigger has a
chance to pair it. The default code-based setup avoids that competition for
the first test.

## Understand replies and media

Channel conversations receive tools to send text, edit messages, react, or
choose to send no message. Those tools are bound to the current conversation.
If no message tool answers, the final assistant text provides the fallback
reply. The current send tool sends text; it cannot send file attachments.

Inbound media has explicit limits:

| Input | Limit per attachment |
| --- | --- |
| JPEG, PNG, WebP, or GIF image | 10 MiB |
| PDF | 10 MiB |
| Text, Markdown, CSV, or JSON document | 1 MiB |
| Supported audio | 25 MiB |

At most eight attachments are admitted per message. Video processing is not
supported. The selected conversational model still needs to support admitted
images and PDFs; channel admission does not add those capabilities to a model
that lacks them.

The connector prepares supported media into content-addressed storage. For
voice messages, the conversation workflow transcribes authorized audio using
the universe's **Speech-to-text** default before delivering the prepared
text to the bot. Configure that route under **Models → Defaults**; the bot's
conversational model does not need audio support. An unset or unusable speech
route prevents voice preparation and is reported as a delivery failure.

Add an execution environment if the bot's task needs processes or a machine's
filesystem.

## Manage connection and conversation state

An existing pairing wins over new routing choices. Changing trigger priority
does not transfer already paired conversations. If the paired bot or trigger
is paused or disabled, the association stays in place and new messages are
not rerouted to another bot. Messages sent to a paused binding are not
buffered for later delivery.

Rotating a pairing code changes the code for future pairings while existing
conversations stay connected. Use **Channels → Connected conversations →
Unpair** to remove a binding. An open matching trigger can pair that chat
again on its next message, so adjust that trigger before relying on unpairing
as a lasting access change.

**Disable** on the channel account stops serving that account through the
connector. Resetting or closing agent conversations is a bot lifecycle
operation; see [Bots and triggers](bots-and-triggers.md#pause-update-reset-or-close).

The Channels page reports **Connected**, **Connecting**, **Waiting for
connector**, **Disconnected**, **Failed**, or **Disabled**. The operator must
configure `LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS` for Platform to aggregate
connector health. Without that connection, the UI can say **Waiting for
connector** even when a bridge is running, so inspect the host's health and
logs as well.

## If a chat does not respond

| Symptom | What to check |
| --- | --- |
| The account remains waiting for a connector | Check enabled providers, account discovery, core and Temporal access, and Platform health URLs. |
| WhatsApp never shows a QR code in the browser | Read the connector process output with QR printing enabled; the web app does not render it. |
| The account is connected but messages have no bot | Create a **Chat account** trigger and send its code as a standalone message. |
| A pairing code reaches the wrong bot | Check for an open matching trigger and any existing pairing, both of which take precedence. |
| Direct chat works but group messages do not | Check the trigger's conversation filter, sender IDs, and mention/reply/prefix activation. |
| Messages disappear while a bot is paused | The binding is retained, but those messages are not queued for replay. Resume the bot and send the task again. |
| Text works but an attachment fails | Check attachment type, size, count, and model input support. |
| The agent answered in Lightspeed but delivery failed | Inspect connector account health and delivery activity, then verify the provider connection is still authenticated. |
