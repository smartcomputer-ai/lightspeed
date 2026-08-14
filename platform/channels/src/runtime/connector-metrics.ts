import type { ConnectorHealthV1 } from "../contracts/connector.js";

export type ConnectorInboundOutcome =
  | "admitted"
  | "paired"
  | "pairing_required"
  | "pairing_pending"
  | "unbound"
  | "rate_limited"
  | "failed";

/** Process-local connector counters rendered on the connector health server. */
export class ConnectorMetrics {
  private readonly inbound = new Map<ConnectorInboundOutcome, number>();

  recordInbound(outcome: ConnectorInboundOutcome): void {
    this.inbound.set(outcome, (this.inbound.get(outcome) ?? 0) + 1);
  }

  render(snapshot: ConnectorHealthV1): string {
    const labels = `provider="${escapeLabel(snapshot.provider)}",account_id="${escapeLabel(snapshot.accountId)}"`;
    const lines = [
      "# HELP channels_connector_ready Whether connector ingress and its activity worker are ready.",
      "# TYPE channels_connector_ready gauge",
      `channels_connector_ready{${labels}} ${snapshot.state === "ready" ? 1 : 0}`,
      "# HELP channels_connector_ingress_connected Whether the provider ingress connection is established.",
      "# TYPE channels_connector_ingress_connected gauge",
      `channels_connector_ingress_connected{${labels}} ${snapshot.ingressConnected ? 1 : 0}`,
      "# HELP channels_connector_activity_worker_ready Whether the provider activity worker is polling.",
      "# TYPE channels_connector_activity_worker_ready gauge",
      `channels_connector_activity_worker_ready{${labels}} ${snapshot.activityWorkerReady ? 1 : 0}`,
      "# HELP channels_connector_reconnect_attempts Current consecutive reconnect attempts.",
      "# TYPE channels_connector_reconnect_attempts gauge",
      `channels_connector_reconnect_attempts{${labels}} ${snapshot.reconnectAttempts}`,
      "# HELP channels_connector_last_error_timestamp_seconds Unix timestamp of the connector's last recorded error.",
      "# TYPE channels_connector_last_error_timestamp_seconds gauge",
      `channels_connector_last_error_timestamp_seconds{${labels}} ${snapshot.lastErrorAtMs === undefined ? 0 : snapshot.lastErrorAtMs / 1_000}`,
      "# HELP channels_connector_inbound_total Normalized provider events by admission outcome.",
      "# TYPE channels_connector_inbound_total counter",
    ];
    for (const outcome of OUTCOMES) {
      lines.push(
        `channels_connector_inbound_total{${labels},outcome="${outcome}"} ${this.inbound.get(outcome) ?? 0}`,
      );
    }
    return `${lines.join("\n")}\n`;
  }
}

const OUTCOMES: readonly ConnectorInboundOutcome[] = [
  "admitted",
  "paired",
  "pairing_required",
  "pairing_pending",
  "unbound",
  "rate_limited",
  "failed",
];

function escapeLabel(value: string): string {
  return value.replaceAll("\\", "\\\\").replaceAll("\n", "\\n").replaceAll('"', '\\"');
}
