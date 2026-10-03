import type { ChannelInboundDecision } from "@lightspeed-ai/client";
import type { ConnectorHostHealth } from "./health.js";
import type { AccountHealth } from "./lifecycle.js";

export type ConnectorInboundOutcome = ChannelInboundDecision | "rate_limited" | "failed";

const OUTCOMES: readonly ConnectorInboundOutcome[] = [
  "bound",
  "paired",
  "pairing_required",
  "pairing_pending",
  "unbound",
  "rate_limited",
  "failed",
];

/** Process-local per-account counters rendered on the host's `/metrics`. */
export class ConnectorMetrics {
  private readonly inbound = new Map<ConnectorInboundOutcome, number>();

  recordInbound(outcome: ConnectorInboundOutcome): void {
    this.inbound.set(outcome, (this.inbound.get(outcome) ?? 0) + 1);
  }

  inboundTotal(outcome: ConnectorInboundOutcome): number {
    return this.inbound.get(outcome) ?? 0;
  }

  /** Render one account's samples; the host renders every account together. */
  render(health: AccountHealth): string {
    return renderConnectorMetrics([{ health, inbound: this }]);
  }
}

export interface ConnectorMetricsEntry {
  health: AccountHealth;
  inbound: ConnectorMetrics;
}

/** Prometheus text exposition of the host and every served account. */
export function renderConnectorMetrics(
  entries: readonly ConnectorMetricsEntry[],
  host?: ConnectorHostHealth,
): string {
  const lines: string[] = [];
  if (host !== undefined) {
    lines.push(
      "# HELP connector_host_ready Whether the host completed discovery and every served account is ready.",
      "# TYPE connector_host_ready gauge",
      `connector_host_ready ${host.state === "ready" ? 1 : 0}`,
      "# HELP connector_host_accounts Accounts currently served by this host.",
      "# TYPE connector_host_accounts gauge",
      `connector_host_accounts ${host.accounts.length}`,
      "# HELP connector_host_discovery_passes_total Completed discovery passes, successful or not.",
      "# TYPE connector_host_discovery_passes_total counter",
      `connector_host_discovery_passes_total ${host.discovery.passes}`,
      "# HELP connector_host_discovery_last_success_timestamp_seconds Unix timestamp of the last successful discovery pass.",
      "# TYPE connector_host_discovery_last_success_timestamp_seconds gauge",
      `connector_host_discovery_last_success_timestamp_seconds ${host.discovery.lastSuccessAtMs === undefined ? 0 : host.discovery.lastSuccessAtMs / 1_000}`,
    );
  }
  const gauge = (name: string, help: string, value: (entry: ConnectorMetricsEntry) => number) => {
    lines.push(`# HELP ${name} ${help}`, `# TYPE ${name} gauge`);
    for (const entry of entries) {
      lines.push(`${name}{${labels(entry.health)}} ${value(entry)}`);
    }
  };
  gauge(
    "channels_connector_ready",
    "Whether connector ingress and its activity worker are ready.",
    ({ health }) => (health.state === "ready" ? 1 : 0),
  );
  gauge(
    "channels_connector_ingress_connected",
    "Whether the provider ingress connection is established.",
    ({ health }) => (health.ingressConnected ? 1 : 0),
  );
  gauge(
    "channels_connector_activity_worker_ready",
    "Whether the provider activity worker is polling.",
    ({ health }) => (health.activityWorkerReady ? 1 : 0),
  );
  gauge(
    "channels_connector_reconnect_attempts",
    "Current consecutive reconnect attempts.",
    ({ health }) => health.reconnectAttempts,
  );
  gauge(
    "channels_connector_last_error_timestamp_seconds",
    "Unix timestamp of the connector's last recorded error.",
    ({ health }) => (health.lastErrorAtMs === undefined ? 0 : health.lastErrorAtMs / 1_000),
  );
  lines.push(
    "# HELP channels_connector_inbound_total Normalized provider events by admission outcome.",
    "# TYPE channels_connector_inbound_total counter",
  );
  for (const entry of entries) {
    for (const outcome of OUTCOMES) {
      lines.push(
        `channels_connector_inbound_total{${labels(entry.health)},outcome="${outcome}"} ${entry.inbound.inboundTotal(outcome)}`,
      );
    }
  }
  return `${lines.join("\n")}\n`;
}

function labels(health: AccountHealth): string {
  return `universe_id="${escapeLabel(health.universeId)}",provider="${escapeLabel(health.provider)}",account_id="${escapeLabel(health.accountId)}"`;
}

function escapeLabel(value: string): string {
  return value.replaceAll("\\", "\\\\").replaceAll("\n", "\\n").replaceAll('"', '\\"');
}
