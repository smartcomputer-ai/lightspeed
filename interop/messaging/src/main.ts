#!/usr/bin/env node
import "dotenv/config";
import { LightspeedClient } from "@lightspeed/agent-client";
import { loadBridgeConfig } from "./config.js";
import { LightspeedSessionBridge } from "./lightspeed.js";
import { OutboxTailer } from "./outbox.js";
import { MessagingBridgeRuntime } from "./runtime.js";
import { JsonBridgeStore } from "./store.js";
import { startTelegramBridge, type RunningBridge } from "./telegram.js";
import { startWhatsAppBridge, type RunningWhatsAppBridge } from "./whatsapp.js";

type Running = RunningBridge | RunningWhatsAppBridge;

const config = await loadBridgeConfig();
const store = new JsonBridgeStore(config.store.path);
const gatewayHeaders: Record<string, string> = {};
if (config.lightspeed.apiKey) {
  gatewayHeaders["authorization"] = `Bearer ${config.lightspeed.apiKey}`;
}
if (config.lightspeed.universe) {
  gatewayHeaders["x-lightspeed-universe"] = config.lightspeed.universe;
}
const client = new LightspeedClient({
  endpoint: config.lightspeed.endpoint,
  headers: gatewayHeaders,
});
const lightspeed = new LightspeedSessionBridge(client, config.lightspeed);
const runtime = new MessagingBridgeRuntime({
  lightspeed,
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

const outbox = new OutboxTailer({
  client,
  store,
  deliverers: running.map((bridge) => bridge.deliverer),
});
outbox.start();

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.once(signal, () => {
    void shutdown(signal);
  });
}

async function shutdown(signal: string): Promise<void> {
  console.log(`bridge: received ${signal}, stopping`);
  await outbox.stop().catch(() => undefined);
  await Promise.allSettled(running.map((bridge) => bridge.stop()));
  await runtime.flush().catch(() => undefined);
  process.exit(0);
}
