import { resolveChannelsRoles, type ChannelsRole } from "./roles.js";
import { installTemporalMetrics } from "./temporal-metrics.js";

const command = process.env.LIGHTSPEED_CHANNELS_ROLE ?? process.argv[2];
const roles = resolveChannelsRoles(command, process.env.CHANNELS_CONNECTORS);
const combined = roles.length > 1;

installTemporalMetrics(combined ? "channels" : `channels-${roles[0]}`, metricsPort(roles));

let stopping = false;
const onSignal = () => {
  stopping = true;
};
process.once("SIGINT", onSignal);
process.once("SIGTERM", onSignal);

console.log(`channels: starting ${roles.join(", ")} in one process`);
const running = roles.map((role) => loadRole(role));

try {
  if (running.length === 1) {
    await running[0];
  } else {
    const stoppedRole = await Promise.race(
      running.map((promise, index) => promise.then(() => roles[index])),
    );
    if (!stopping) {
      throw new Error(`Channels ${stoppedRole} role stopped while other roles were running`);
    }
    await Promise.all(running);
  }
} finally {
  process.off("SIGINT", onSignal);
  process.off("SIGTERM", onSignal);
}

function loadRole(role: ChannelsRole): Promise<unknown> {
  switch (role) {
    case "workflows":
      return import("./workflow-worker.js");
    case "activities":
      return import("./activity-worker.js");
    case "telegram":
      return import("./telegram-worker.js");
    case "whatsapp":
      return import("./whatsapp-worker.js");
  }
}

function metricsPort(rolesToRun: readonly ChannelsRole[]): number {
  if (rolesToRun.length > 1) return 9_090;
  const role = rolesToRun[0];
  if (role === undefined) {
    throw new TypeError("at least one Channels role is required");
  }
  switch (role) {
    case "workflows":
      return 9_090;
    case "telegram":
      return 9_091;
    case "whatsapp":
      return 9_092;
    case "activities":
      return 9_093;
  }
}
