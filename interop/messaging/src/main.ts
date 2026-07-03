#!/usr/bin/env node
import "dotenv/config";
import { loadBridgeConfig } from "./config.js";
import { buildGatewayConnections } from "./gateway_auth.js";
import { OutboxTailer } from "./outbox.js";
import { MessagingBridgeRuntime } from "./runtime.js";
import { JsonBridgeStore } from "./store.js";
import { startTelegramBridge, type RunningBridge } from "./telegram.js";
import { startWhatsAppBridge, type RunningWhatsAppBridge } from "./whatsapp.js";

type Running = RunningBridge | RunningWhatsAppBridge;

const config = await loadBridgeConfig();
const store = new JsonBridgeStore(config.store.path);
// One gateway connection per distinct credential: the default plus any
// per-binding auth (P90 multi-tenancy).
const connections = buildGatewayConnections(config);
const runtime = new MessagingBridgeRuntime({
  lightspeed: connections.default.lightspeed,
  lightspeedByAuthBindingId: new Map(
    [...connections.byBindingId].map(([bindingId, connection]) => [
      bindingId,
      connection.lightspeed,
    ]),
  ),
  store,
  runtime: config.runtime,
  sessionPrefix: config.lightspeed.sessionPrefix,
});
const running: Running[] = [];
const routing = { bindings: config.bindings };

if (config.telegram?.enabled) {
  running.push(await startTelegramBridge(config.telegram, runtime, routing));
}

if (config.whatsapp?.enabled) {
  running.push(await startWhatsAppBridge(config.whatsapp, runtime, routing));
}

if (running.length === 0) {
  throw new Error("No bridge is enabled. Set TELEGRAM_BOT_TOKEN or WHATSAPP_ENABLED=true.");
}

// The outbox is universe-scoped: one tailer per distinct credential, each
// with its own client and cursor. Connections deduplicate by credential, so
// no universe outbox is ever read by two tailers (which would double-deliver).
const deliverers = running.map((bridge) => bridge.deliverer);
const outboxes = connections.distinct.map(
  (connection) =>
    new OutboxTailer({
      client: connection.client,
      store,
      deliverers,
    }),
);
for (const outbox of outboxes) {
  outbox.start();
}

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.once(signal, () => {
    void shutdown(signal);
  });
}

async function shutdown(signal: string): Promise<void> {
  console.log(`bridge: received ${signal}, stopping`);
  await Promise.allSettled(outboxes.map((outbox) => outbox.stop()));
  await Promise.allSettled(running.map((bridge) => bridge.stop()));
  await runtime.flush().catch(() => undefined);
  process.exit(0);
}
