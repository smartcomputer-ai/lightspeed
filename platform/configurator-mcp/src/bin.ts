#!/usr/bin/env node
import { configFromEnv } from "./config.js";
import { startConfigurator } from "./transport.js";

const config = configFromEnv();
const running = await startConfigurator({ config });
process.stderr.write(`Lightspeed Configurator MCP listening on ${running.url}/mcp\n`);

let stopping = false;
async function stop(): Promise<void> {
  if (stopping) {
    return;
  }
  stopping = true;
  await running.close();
}

process.on("SIGINT", () => {
  void stop();
});
process.on("SIGTERM", () => {
  void stop();
});
