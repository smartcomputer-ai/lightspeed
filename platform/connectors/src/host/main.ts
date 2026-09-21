import { NativeConnection } from "@temporalio/worker";
import { CoreClient } from "../core/client.js";
import { AccountRunner } from "./account-runner.js";
import { parseHostConfig } from "./config.js";
import { ConnectorHost } from "./host.js";
import { installTemporalMetrics } from "./temporal-metrics.js";

const config = parseHostConfig(process.env);
installTemporalMetrics("lightspeed-connectors", config.metrics);
const core = new CoreClient({ endpoint: config.apiUrl, apiKey: config.apiKey });
const connection = await NativeConnection.connect({ address: config.temporal.address });
const host = new ConnectorHost(
  {
    providers: config.providers,
    accounts: config.accounts,
    discoveryIntervalMs: config.discoveryIntervalMs,
    health: config.health,
  },
  {
    core,
    createRunner: (account) =>
      new AccountRunner(account, {
        core,
        temporal: { connection, namespace: config.temporal.namespace },
        whatsapp: config.whatsapp,
        ingressMaxPerMinute: config.ingressMaxPerMinute,
      }),
  },
);

const signalled = new Promise<NodeJS.Signals>((resolve) => {
  process.once("SIGINT", () => resolve("SIGINT"));
  process.once("SIGTERM", () => resolve("SIGTERM"));
});

try {
  await host.start();
  console.log(
    `connectors: host serving ${config.providers.join(", ")}${
      config.accounts === null
        ? ""
        : ` for ${config.accounts.map((a) => `${a.universeId}/${a.accountId}`).join(", ")}`
    } from ${config.apiUrl}; health on port ${host.healthPort}, Temporal metrics on port ${config.metrics.port}`,
  );
  const signal = await signalled;
  console.log(`connectors: ${signal} received; stopping`);
} finally {
  await host.stop();
  await connection.close();
}
