import { Runtime } from "@temporalio/worker";

let installed = false;

/**
 * Install the process-wide Temporal Prometheus exporter before the first
 * NativeConnection is created. Split mode installs once in each role process;
 * combined mode installs once before loading any of the role modules.
 */
export function installTemporalMetrics(service: string, fallbackPort: number): void {
  if (installed) return;
  const host = process.env.CHANNELS_METRICS_HOST ?? "0.0.0.0";
  const port = parseMetricsPort(process.env.CHANNELS_METRICS_PORT, fallbackPort);
  Runtime.install({
    telemetryOptions: {
      metrics: {
        prometheus: {
          bindAddress: `${host}:${port}`,
          countersTotalSuffix: true,
          unitSuffix: true,
          useSecondsForDurations: true,
        },
        globalTags: { service },
      },
    },
  });
  installed = true;
}

export function parseMetricsPort(value: string | undefined, fallback: number): number {
  const port = value === undefined ? fallback : Number(value);
  if (!Number.isSafeInteger(port) || port < 1 || port > 65_535) {
    throw new TypeError("Channels metrics port must be an integer between 1 and 65535");
  }
  return port;
}
