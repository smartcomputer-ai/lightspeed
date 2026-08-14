import type { ChannelProvider, NormalizedInboundV1 } from "./channel.js";
import type { ChannelDeliveryActivities } from "./delivery.js";

export type ConnectorHealthState =
  | "starting"
  | "ready"
  | "disconnected"
  | "stopping"
  | "stopped";

export interface ConnectorHealthV1 {
  version: 1;
  provider: ChannelProvider;
  accountId: string;
  state: ConnectorHealthState;
  ingressConnected: boolean;
  activityWorkerReady: boolean;
  reconnectAttempts: number;
  detail?: string;
  lastError?: string;
  lastErrorAtMs?: number;
  changedAtMs: number;
}

export type ChannelIngressHandler = (inbound: NormalizedInboundV1) => Promise<void>;

/** Provider process boundary. Live clients never cross this interface as payloads. */
export interface ChannelConnector {
  readonly provider: ChannelProvider;
  readonly accountId: string;
  readonly deliveryActivities: ChannelDeliveryActivities;
  startIngress(handler: ChannelIngressHandler): Promise<void>;
  health(): ConnectorHealthV1;
  stop(): Promise<void>;
}
