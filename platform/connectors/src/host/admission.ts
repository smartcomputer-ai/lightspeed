import type {
  ChannelInbound,
  ChannelInboundDecision,
  LightspeedClient,
} from "@lightspeed-ai/sdk";
import type { InboundGate, InboundVerdict } from "../providers/connector.js";
import type { ConnectorMetrics } from "./metrics.js";
import type { FixedWindowRateLimiter } from "./rate-limit.js";

export const PAIRING_REQUIRED_REPLY =
  "This chat is not paired yet. Send the pairing code to connect it.";
export const PAIRING_CONFIRMED_REPLY =
  "Paired. You can now message Lightspeed from this chat.";

/** What the host says back to the chat for a core decision; the provider quotes the message. */
export function pairingReplyFor(decision: ChannelInboundDecision): string | null {
  switch (decision) {
    case "paired":
      return PAIRING_CONFIRMED_REPLY;
    case "pairing_required":
      return PAIRING_REQUIRED_REPLY;
    case "bound":
    case "pairing_pending":
    case "unbound":
      return null;
  }
}

export interface InboundGateOptions {
  /** Universe-scoped core client of the account. */
  client: Pick<LightspeedClient, "call">;
  accountId: string;
  rateLimit: FixedWindowRateLimiter;
  metrics: ConnectorMetrics;
  log?: Pick<Console, "warn" | "error">;
}

/**
 * The host side of ingress: per-chat rate limiting, then
 * `channels/inbound/admit` stamped with the account's universe. The verdict
 * resolves only after the core answered, so the provider acknowledges the
 * message after the core holds it. A failure never throws into the provider
 * loop; it is counted and answered with no reply.
 */
export function createInboundGate(options: InboundGateOptions): InboundGate {
  const log = options.log ?? console;
  return {
    async admit(inbound: ChannelInbound): Promise<InboundVerdict> {
      if (!options.rateLimit.allow(`${inbound.chatId}\0${inbound.senderId}`)) {
        options.metrics.recordInbound("rate_limited");
        log.warn(`connectors: ${options.accountId} ingress rate limited chat ${inbound.chatId}`);
        return { outcome: "rate_limited", reply: null };
      }
      let decision: ChannelInboundDecision;
      try {
        const response = await options.client.call("channels/inbound/admit", {
          accountId: options.accountId,
          inbound,
        });
        decision = response.result.decision;
      } catch (error) {
        options.metrics.recordInbound("failed");
        log.error(`connectors: ${options.accountId} inbound admission failed`, error);
        return { outcome: "failed", reply: null };
      }
      options.metrics.recordInbound(decision);
      if (decision === "unbound") {
        log.warn(`connectors: ${options.accountId} ignored unbound chat ${inbound.chatId}`);
      }
      return { outcome: decision, reply: pairingReplyFor(decision) };
    },
  };
}
