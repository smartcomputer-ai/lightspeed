import { sha256 } from "@noble/hashes/sha2.js";
import type { ChannelProvider, ChannelRoute } from "../contracts/channel.js";

// Lightspeed's long-lived default universe uses the canonical UUID text shape
// with a zero version nibble (`00000000-0000-0000-0000-000000000001`).
// Universe ids are opaque tenant identifiers here, so validate their shape
// without imposing RFC version/variant bits that Lightspeed itself does not.
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export interface ChannelIdentityInput {
  universeId: string;
  provider: ChannelProvider;
  accountId: string;
  sessionKey: string;
}

export interface ChannelSessionIdentity {
  workflowId: string;
  sessionId: string;
  deliveryTaskQueue: string;
}

export interface ChannelTurnIdentity {
  submissionId: string;
  terminalToken: string;
}

export function channelSessionIdentity(input: ChannelIdentityInput): ChannelSessionIdentity {
  if (!UUID.test(input.universeId)) {
    throw new TypeError("universeId must be a UUID");
  }
  requirePart(input.accountId, "accountId");
  requirePart(input.sessionKey, "sessionKey");

  const routeHash = digest("lightspeed.channels.session.v1", [
    input.universeId.toLowerCase(),
    input.provider,
    input.accountId,
    input.sessionKey,
  ]);

  return {
    workflowId: `lightspeed.channels.v1/${input.universeId.toLowerCase()}/${input.provider}/${routeHash}`,
    sessionId: `channel:v1:${input.provider}:${routeHash}`,
    deliveryTaskQueue: channelDeliveryTaskQueue(input.provider, input.accountId),
  };
}

export function channelDeliveryTaskQueue(
  provider: ChannelProvider,
  accountId: string,
): string {
  requirePart(accountId, "accountId");
  const accountHash = digest("lightspeed.channels.account.v1", [provider, accountId]).slice(0, 24);
  return `lightspeed-channels-delivery-v1-${provider}-${accountHash}`;
}

export function lightspeedSessionWorkflowId(universeId: string, sessionId: string): string {
  if (!UUID.test(universeId)) {
    throw new TypeError("universeId must be a UUID");
  }
  requirePart(sessionId, "sessionId");
  if (sessionId.includes("/")) {
    throw new TypeError("sessionId must not contain '/'");
  }
  return `${universeId.toLowerCase()}/${sessionId}`;
}

export function channelTurnIdentity(workflowId: string, providerMessageId: string): ChannelTurnIdentity {
  requirePart(workflowId, "workflowId");
  requirePart(providerMessageId, "providerMessageId");
  const turnHash = digest("lightspeed.channels.turn.v1", [workflowId, providerMessageId]);
  return {
    submissionId: `channels-v1-${turnHash}`,
    terminalToken: `channels-terminal-v1-${turnHash}`,
  };
}

export function channelContextKey(inboundKey: string): string {
  requirePart(inboundKey, "inboundKey");
  return `channel.room.v1.${digest("lightspeed.channels.context.v1", [inboundKey])}`;
}

export function channelPairingKey(route: ChannelRoute): string {
  requirePart(route.accountId, "accountId");
  requirePart(route.chatId, "chatId");
  return `channels-pairing-v1-${digest("lightspeed.channels.pairing.v1", [
    route.provider,
    route.accountId,
    route.chatId,
  ])}`;
}

function digest(domain: string, parts: readonly string[]): string {
  const hash = sha256.create();
  for (const part of [domain, ...parts]) {
    const bytes = new TextEncoder().encode(part);
    const length = new Uint8Array(8);
    new DataView(length.buffer).setBigUint64(0, BigInt(bytes.length), false);
    hash.update(length);
    hash.update(bytes);
  }
  return Array.from(hash.digest(), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function requirePart(value: string, name: string): void {
  if (value.length === 0) {
    throw new TypeError(`${name} must not be empty`);
  }
}
