# Implement a channel connector

A channel connector translates between a chat provider and Lightspeed. It
receives provider messages, normalizes their identity and
content, and delivers runtime-requested responses. The core owns pairing,
sender policy, bot routing, sessions, and durable conversation state.

The included Telegram and WhatsApp connectors share one Node host. Adding
another transport usually means implementing the same connector lifecycle and
activity contract, then teaching the host to construct it. A connector needs
API and Temporal access; it does not read Lightspeed's database or make its
own bot-routing decisions.

## Follow the message path

The host discovers channel accounts with
`deployment/channels/accounts/list`. For each selected account it creates a
universe-scoped API client, starts provider ingress, and runs a Temporal
activity worker on that account's queue.

An incoming message becomes a `ChannelInbound` request to
`channels/inbound/admit`. The core decides whether it is paired, allowed, and
routable. If a bot processes it, the conversation workflow later schedules
connector activities to prepare media, show typing, or deliver a response.

The shipped host authenticates with `LIGHTSPEED_CONNECTOR_API_KEY` against an
`authenticated` runtime. It needs a deployment-scoped key with the
`deployment/channels`, `auth/lease`, `channels/inbound`, and `blobs/put` method
groups. Account calls select their universe with `x-lightspeed-universe`;
deployment-wide account discovery omits that header.

Keep that endpoint and Temporal access on the deployment's private service
network. [API keys and service access](../access-and-security/api-keys-and-service-access.md)
explains scope and method groups; [Chat channels](../using-lightspeed/chat-channels.md)
explains the user-facing account and pairing flows.

## Add a provider implementation

Suppose an installation needs its internal `acorn-chat` service. The core's
provider identifier is an open validated slug, so another provider name does
not inherently require a new Rust enum variant. Account settings also carry
provider-specific non-secret fields.

The shipped Node host has an explicit provider allowlist and factory. Extend
those alongside the implementation:

1. Add the provider directory with ingress normalization, delivery, media, and
   presence behavior as required by the service.
2. Implement the `ProviderConnector` interface: its activities, long-running
   `run()` lifecycle, and `stop()` cleanup.
3. Add construction to the provider factory and the name to the host's
   supported-provider configuration.
4. Add credentials and non-secret settings needed to construct a provider
   session, keeping actual provider clients inside the connector process.
5. Add focused tests for transport behavior and lifecycle before enabling the
   provider in a deployment.

The [connector interface](../../../platform/connectors/src/providers/connector.ts),
[account runner and factory](../../../platform/connectors/src/host/account-runner.ts),
and [Telegram implementation](../../../platform/connectors/src/providers/telegram/index.ts)
provide starting points. Reuse the shared account runner and admission code
instead of creating a separate worker supervision or routing system.

If the Platform should offer a first-class connection form, extend its
[account setup route](../../../platform/server/src/routes/channel-accounts.ts)
and [Channels page](../../../platform/web/src/pages/ChannelsPage.tsx) as well.
That flow should validate the provider account identity and store credentials
through the credential API. A new core wire field or operation requires contract
regeneration; a new provider name using existing fields does not by itself
require changing generated clients.

## Normalize ingress without losing identity

Preserve the provider's stable message, chat, thread, and sender identifiers.
Include the provider timestamp, available display identity, text/caption, and
the facts needed for direct/group activation, such as a direct mention or reply
to the bot. Ignore self-originated messages to avoid feeding the connector's
own responses back into the agent.

Retries must preserve the original provider message identity. Generating a
new message ID on every reconnect defeats the core's bounded deduplication
window. Admission is not an exactly-once delivery guarantee, so also decide
when the provider's update is acknowledged and how failed admission is retried.

Use the shared [admission gate](../../../platform/connectors/src/host/admission.ts)
for per-chat/per-sender rate limiting and core admission. It maps decisions
to the appropriate provider response: pairing confirmation or a pairing prompt
where required, and silence for decisions that need no transport response.
Its failed-admission result is returned as `failed` rather than always thrown;
the adapter must account for that when advancing a provider cursor or
acknowledging a webhook.

Attachments enter as metadata and provider locators: file ID, kind, MIME type,
name, and declared size. Do not download all media before admission or insert
provider tokens and binary payloads into `ChannelInbound`. The core can request
media preparation later through a connector activity.

Test direct messages, groups, threads, mentions, replies, captions, attachments,
duplicate deliveries, and self messages. The existing
[Telegram normalizer](../../../platform/connectors/src/providers/telegram/ingress.ts)
and [WhatsApp normalizer](../../../platform/connectors/src/providers/whatsapp/ingress.ts)
show how different provider payloads map into the common contract.

<a id="resolve-credentials-at-the-service-boundary"></a>

## Resolve credentials in the connector

An account's `credentialGrantId` points to a stored retrievable grant. The
service client leases it through `auth/grants/lease` using its key's `auth/lease`
permission in the account's universe. Provider tokens are not ordinary account
settings or workflow arguments.

The existing grant lease cache retains a credential for at most five minutes,
or less when expiry is near, and can be invalidated after provider authentication
failure. Reuse that cache when a connector needs token renewal. A stale
provider session should not keep using a cached token indefinitely after a
credential change.

Some transports also have local authentication state. WhatsApp stores linked
device state under a universe/account directory and seals media locators with
a stable deployment key. A new provider may need its own persisted state, but
that does not require access to the Lightspeed database. Document its backup,
move, revocation, and shutdown behavior alongside the provider's configuration.

## Implement the connector activities

The generated workflow contract defines three activity names:

| Activity | Responsibility |
| --- | --- |
| `deliverChannelMessage` | Execute versioned send/edit/react operations and return provider message IDs. |
| `prepareChannelMedia` | Validate ownership, resolve/download the attachment, enforce limits, upload through `blobs/put`, and return its reference/metadata. |
| `maintainChannelTyping` | Maintain best-effort typing presence, heartbeat, observe cancellation, and clear presence during cleanup. |

Import their names from `CHANNEL_CONNECTOR_ACTIVITIES` and derive the account
queue with the shared helper, rather than copying the hashing algorithm:

```ts
import { connectorTaskQueue } from "@lightspeed-ai/agent-client/workflow";

const queue = connectorTaskQueue(
  "00000000-0000-0000-0000-000000000001",
  "acorn-chat",
  "support",
);
```

This is an illustrative account identity. Use the discovered universe,
provider, and account values in the actual worker. All participants must use
the same Temporal namespace and derived queue. Keep custom connector activities
on that queue, not the runtime's session or core-channel queue.

<a id="deliver-replies-with-explicit-retry-semantics"></a>

### Deliver replies safely under retries

`ChannelDeliveryCommand` carries a contract version, invocation ID, idempotency
key, route, and operation. Verify the version and route's provider/account
against the worker and preserve invocation/idempotency identity. The account's
queue and worker context establish the universe; this command does not carry
a separate universe field. The command uses actual provider message IDs;
the core resolves model-visible handles such as `#N` before invoking the
connector.

The core schedules split message chunks durably, and a connector may need
additional splitting for provider limits. Preserve the invocation/chunk
identity when retrying. An idempotency key in the command does not make the
provider's API idempotent: a send can succeed while its response is lost.
Use provider idempotency support or an appropriate reconciliation strategy
where available, and document remaining duplicate-delivery behavior.

Classify invalid input or route ownership as permanent errors. Distinguish
those from transient network/provider failures and authentication that can
be refreshed. An unqualified retry of every failure can repeat messages or
hide a configuration problem.

### Prepare media and presence separately

Media preparation does carry a universe ID. Verify it matches the account
worker, resolve attachment locators using that account's credential, and
enforce allowed MIME and size limits while downloading. Upload the
accepted content through the universe API, then return its blob reference and
metadata. Large blobs require the deployment's configured object store; the
connector should not bypass storage policy by writing directly to a bucket.

Typing is best-effort transport presence. Keep it responsive to Temporal
cancellation, heartbeat while maintaining it, and clear it in cleanup. A
typing loop should neither become the source of durable conversation truth
nor prevent account shutdown.

## Integrate account lifecycle and health

The shared account runner starts both ingress and the activity worker. If
either fails, it stops the other side and marks the account failed. Discovery
starts new accounts, stops removed/disabled ones, and restarts accounts whose
document revision changed or whose runner failed.

Report connection, disconnection, and scheduled reconnects through the host's
existing status hooks. Account readiness requires both connected ingress and
a ready activity worker. Host health and metrics then expose the same
operational view for all providers.

In `stop()`, terminate subscriptions, polling, sockets, timers, and provider
resources owned by the connector. Coordinate this with the shared activity
worker's shutdown. Persist only the state the provider actually needs to
resume its identity or cursor, and keep it scoped by universe/account.

There is no cross-host account ownership election. Partition providers or
accounts so only one ingress owner consumes a given provider account. The
[operations guide](../deployment/operations.md#partition-connector-accounts)
also explains the limits of `/readyz`: an empty account inventory can be ready,
and later discovery failure does not necessarily make existing accounts unready.

## Verify before enabling real traffic

Use provider fixtures and fake API clients and activities to test normalization,
route rejection, credential refresh, duplicate delivery, media limits, typing
cancellation, reconnects, and account revision changes. The existing suite
runs without real chat credentials:

```bash
npm run test --workspace @lightspeed/connectors
```

Then enable one controlled account. Verify discovery and readiness, pairing,
one admitted message, the bot run, and the returned provider message. Test
attachments and group policy where supported. Restart the host and disable
the account to confirm both transport ingress and activity work follow the
intended lifecycle.

| Symptom | What to inspect |
| --- | --- |
| The new provider never starts | Host allowlist, factory, configured selection, and discovered account record. |
| Ingress connects but admission fails | Authenticated endpoint, key scope and `channels/inbound` group, universe mapping, and normalized IDs. |
| Inbound works but no replies arrive | Derived activity queue, registered contract names, worker readiness, and provider delivery errors. |
| Reconnect repeats messages | Provider acknowledgment/cursor handling and stable message IDs. |
| Removed accounts keep receiving work | Runner shutdown, subscriptions/timers, and duplicate host ownership. |

Use the [generated workflow contract](../../../crates/temporal-workflow/contract/workflow-contract.md)
for exact activity payloads and the [connector source guide](../../../platform/connectors/README.md)
for host configuration. Keep the transport implementation thin so changes to
bot routing and session policy remain in the core.
