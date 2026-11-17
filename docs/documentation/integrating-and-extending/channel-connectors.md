# Implement a channel connector

A connector translates between a chat provider and Lightspeed. It receives
messages, normalizes their identity and content, and delivers replies. The
Lightspeed core handles pairing, sender policy, bot routing, and session
state. The Telegram and WhatsApp connectors share one Node host.

## Follow the message path

The connector host uses the private runtime HTTP API and the same Temporal
namespace as Lightspeed. Run it separately from the development launcher
with a full-authority runtime token. The host uses that credential for
account discovery and selects the universe for each account operation. See
the [connector host guide](../../../platform/connectors/README.md) for
manual startup, credentials, and account selection.

For each discovered account, the host starts provider ingress and a Temporal
activity worker on the account's queue. It submits normalized inbound messages
to `channels/inbound/admit`. If a message starts a bot run, the conversation
workflow can ask the connector to prepare an attachment, show typing, or
deliver a reply.

## Add a provider implementation

Implement the [ProviderConnector interface](../../../platform/connectors/src/providers/connector.ts)
with `run()`, `stop()`, and the activities needed by the transport. Add the
provider to the host's [configuration](../../../platform/connectors/src/host/config.ts)
and [factory](../../../platform/connectors/src/host/account-runner.ts).
Reuse the account runner to supervise ingress and the activity worker.
The [Telegram implementation](../../../platform/connectors/src/providers/telegram/index.ts)
provides an example.

Provider names and provider-specific account settings use the existing
channel contract. If you add a connection form, validate the provider account
and store its secret through the credential API. See the
[account setup routes](../../../platform/backend/src/routes/channel-accounts.ts).
Regenerate the contracts when changing public fields or operations.

## Normalize ingress without losing identity

Keep the provider's stable message, chat, thread, and sender IDs, including
on reconnect. Include timestamps, text or captions, and the activation
signals the transport provides, such as a direct mention or a reply to the
bot. Ignore messages originating from the connector itself.

Use the shared [admission gate](../../../platform/connectors/src/host/admission.ts)
for rate limiting and pairing responses. Handle failed admission before
advancing a provider cursor or acknowledging a webhook.

Send attachment locators and metadata with the message. Download the bytes
when the core requests media preparation. The
[Telegram](../../../platform/connectors/src/providers/telegram/ingress.ts) and
[WhatsApp](../../../platform/connectors/src/providers/whatsapp/ingress.ts)
normalizers show how to map different transport payloads to the common
contract.

<a id="resolve-credentials-at-the-service-boundary"></a>

## Resolve credentials in the connector

When an account names a `credentialGrantId`, use the host's lease client to
obtain the provider credential from that universe. Reuse the grant cache and
invalidate it after a provider authentication failure. Keep provider secrets
out of account settings and workflow payloads.

If the transport keeps local state, scope it to the universe and account.
WhatsApp, for example, stores linked device state separately for each
account. Document how to resume, move, or revoke that state.

## Implement the connector activities

| Activity | Responsibility |
| --- | --- |
| `deliverChannelMessage` | Send, edit, or react, and return provider message IDs. |
| `prepareChannelMedia` | Resolve the attachment for this account, enforce size and MIME limits, and return the reference to the uploaded blob. |
| `maintainChannelTyping` | Maintain presence while requested, heartbeat, and clear presence on cancellation or completion. |

Use the generated `CHANNEL_CONNECTOR_ACTIVITIES` names and
`connectorTaskQueue(universeId, provider, accountId)` from
`@lightspeed-ai/sdk/workflow`. Register the activities on that account's
queue. The [workflow contract](../../../crates/temporal-workflow/contract/workflow-contract.md)
specifies the payloads.

<a id="deliver-replies-with-explicit-retry-semantics"></a>

### Deliver replies safely under retries

Validate the version and provider-account route of each delivery. Preserve
the invocation and idempotency IDs across retries, including for message
chunks. A provider can accept a message before the connector receives its
response. Use the provider's idempotency or reconciliation facilities when
available, and document when a retry can deliver a duplicate.

### Prepare media and presence separately

Check the media request's universe against the account worker. Resolve the
locator with that account's credential and upload accepted bytes through the
universe API. For typing, observe cancellation promptly so presence work can
finish during account shutdown.

## Integrate account lifecycle and health

Report readiness after ingress connects and the activity worker is ready.
Close subscriptions, sockets, timers, and provider clients in `stop()`.
Discovery restarts a runner when its account configuration changes or its
runner fails.

Give each provider account one ingress owner. See
[Partition connector accounts](../deployment/operations.md#partition-connector-accounts)
for host selection and health checks.

## Verify before enabling real traffic

Test normalization, retry identities, media limits, reconnects, and shutdown
with fake providers and API clients.

```bash
npm run test --workspace @lightspeed-ai/connectors
```

Then try one enabled account. Verify discovery, pairing, an inbound message
that reaches the bot, and the provider's reply. Test an attachment and check
that disabling the account stops its runner.
