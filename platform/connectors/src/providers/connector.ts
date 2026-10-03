import type { ChannelInbound, ChannelInboundDecision } from "@lightspeed-ai/client";
import type {
  ChannelDeliveryCommand,
  ChannelDeliveryResult,
  MaintainChannelTypingInput,
  PrepareChannelMediaInput,
  PrepareChannelMediaResult,
} from "@lightspeed-ai/client/workflow";

/** The three activities a connector serves on its account's task queue, by manifest name. */
export interface ConnectorActivities {
  deliverChannelMessage(command: ChannelDeliveryCommand): Promise<ChannelDeliveryResult>;
  prepareChannelMedia(input: PrepareChannelMediaInput): Promise<PrepareChannelMediaResult>;
  maintainChannelTyping(input: MaintainChannelTypingInput): Promise<void>;
}

export type InboundOutcome = ChannelInboundDecision | "rate_limited" | "failed";

export interface InboundVerdict {
  outcome: InboundOutcome;
  /** Text the provider sends back into the chat, quoting the message, when the core asked for one. */
  reply: string | null;
}

/** The host side of ingress: resolves once the core holds the message, so the provider may acknowledge it. */
export interface InboundGate {
  admit(inbound: ChannelInbound): Promise<InboundVerdict>;
}

/** What a provider reports about its ingress connection. */
export interface IngressHealth {
  markIngressConnected(): void;
  markIngressDisconnected(detail: string): void;
  markReconnectScheduled(detail: string): void;
}

/**
 * Provider process boundary. Live clients (a grammy `Bot`, a Baileys socket)
 * never cross this interface as payloads; the host sees ingress as a long
 * running `run()` and the account's activities.
 */
export interface ProviderConnector {
  readonly activities: ConnectorActivities;
  /** Run provider ingress until `stop()`; rejects on an unrecoverable provider failure. */
  run(): Promise<void>;
  stop(): Promise<void>;
}
