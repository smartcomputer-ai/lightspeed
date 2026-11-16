# Lightspeed connector host

`@lightspeed-ai/connectors` bridges chat providers (Telegram, WhatsApp) to the
Lightspeed core. It is **one process serving many accounts across many
universes**: one grammy long-poller or Baileys socket per account, one Temporal
activity worker per account queue, all in one Node process. Its only
dependencies are the core HTTP API and Temporal; it reads no database.

The development launcher does not start the host. Trusted operators can run it
manually with an explicit `LIGHTSPEED_CONNECTOR_API_TOKEN`. This is a full
authority runtime credential; the shared browser API does not expose connector
service operations. Scoped Platform service credentials remain separate work.

## What it does

1. **Discovery.** Every `LIGHTSPEED_CONNECTOR_DISCOVERY_INTERVAL_MS` the host
   calls `deployment/channels/accounts/list { includeDisabled: false }`, keeps the
   accounts of its providers (`LIGHTSPEED_CONNECTOR_PROVIDERS`) and, when set,
   of `LIGHTSPEED_CONNECTOR_ACCOUNTS`, and reconciles the running set: new
   accounts start, missing or disabled ones stop, a changed document revision
   or a failed runner restarts (`src/host/discovery.ts` is the pure planner).
2. **Per-account runner** (`src/host/account-runner.ts`). A Telegram account
   leases its bot token through `credentials/grants/lease { grantId: credentialGrantId }`
   (cached in memory until `expiresAtMs - 30 s`, at most five minutes,
   re-leased after Telegram answers 401). A WhatsApp account keeps its Baileys
   session under `LIGHTSPEED_CONNECTOR_WHATSAPP_AUTH_DIR/<universeId>/<accountId>`
   and pairs by QR code unless `settings.printQr` is false. The runner starts
   the provider ingress and a Temporal `Worker` on
   `connectorTaskQueue(universeId, provider, accountId)` (derived exactly as the
   workflow contract specifies, asserted against the contract's known-answer
   vector) with the three manifest activities `deliverChannelMessage`,
   `prepareChannelMedia`, and `maintainChannelTyping`.
3. **Inbound.** Each provider message is normalized into the core's
   `ChannelInbound`, rate limited per chat and sender, and handed to
   `channels/inbound/admit { accountId, inbound }` stamped with the account's
   universe. The decision drives what the host sends back itself: `paired`
   gets the pairing confirmation, `pairingRequired` the pairing prompt;
   `bound`, `pairingPending`, and `unbound` stay silent. The provider is
   acknowledged only after the core answered.
4. **Health and metrics.** One listener (`LIGHTSPEED_CONNECTOR_HEALTH_PORT`,
   default 8090) serves `/healthz`, `/readyz` (200 only when discovery
   succeeded and every served account is ready), and `/metrics` with
   per-account samples labelled by universe, provider, and account. The
   Temporal SDK's Prometheus exporter binds `LIGHTSPEED_CONNECTOR_METRICS_PORT`
   (default 9090).

## Runtime access

Account requests address `/api/v1/universes/{universe}` in the runtime HTTP API.
The host supplies its configured bearer token and never uses a universe header.
Discovery, credential leases and inbound admission require a trusted runtime
connection; they are deliberately unavailable through Platform's browser API.

For implementing another provider, read
[Channel connectors](../../docs/documentation/integrating-and-extending/channel-connectors.md)
in the product manual.

## Configuration

See the "Connector host" section of [environment-variable reference](../../docs/documentation/reference/environment-variables.md).
Configuration uses `LIGHTSPEED_API_URL` ending in `/api/v1` and
`LIGHTSPEED_CONNECTOR_API_TOKEN`; WhatsApp additionally needs
`LIGHTSPEED_CONNECTOR_WHATSAPP_AUTH_DIR` and
`LIGHTSPEED_CONNECTOR_WHATSAPP_MEDIA_LOCATOR_KEY`.

The Temporal connection (`src/host/temporal-connection.ts`) reads
`TEMPORAL_ADDRESS` and `TEMPORAL_NAMESPACE` and, for a frontend that requires
them, the `TEMPORAL_TLS*`, `TEMPORAL_API_KEY`, and `TEMPORAL_API_KEY_FILE`
settings. The runtime reads the same variables with the same rules; the
reference's "Temporal connection security" section lists them.

```bash
npm run test --workspace @lightspeed-ai/connectors
npm run typecheck --workspace @lightspeed-ai/connectors
```

Setting `LIGHTSPEED_CHANNELS_CONNECTORS` does not enable the disabled host.

## Layout

- `src/host/` — entry point, configuration, discovery planner, account runner,
  inbound admission, health/metrics, rate limiting.
- `src/core/` — the core client (universe selection), grant
  leases, account identities.
- `src/providers/telegram/`, `src/providers/whatsapp/` — provider ingress,
  delivery, media, and presence over the shared `ProviderConnector` seam.
- `src/presentation/`, `src/media/` — Markdown rendering per provider, message
  splitting, media MIME/size admission.
