import { describe, expect, it } from "vitest";
import {
  channelSessionIdentity,
  channelPairingKey,
  channelDeliveryTaskQueue,
  channelTurnIdentity,
  lightspeedSessionWorkflowId,
} from "../src/identity/ids.js";

const universeId = "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f";
const defaultUniverseId = "00000000-0000-0000-0000-000000000001";

describe("Channels identities", () => {
  it("derives stable, opaque workflow, session, and account queue ids", () => {
    const input = {
      universeId,
      provider: "whatsapp" as const,
      accountId: "41790000000@s.whatsapp.net",
      sessionKey: "family",
    };
    const first = channelSessionIdentity(input);
    const retry = channelSessionIdentity(input);

    expect(retry).toEqual(first);
    expect(first.workflowId).toMatch(
      /^lsbot\.channels\.v1\/6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f\/whatsapp\/[0-9a-f]{64}$/,
    );
    expect(first.sessionId).toMatch(/^channel:v1:whatsapp:[0-9a-f]{64}$/);
    expect(first.deliveryTaskQueue).toMatch(/^lsbot-channels-delivery-v1-whatsapp-[0-9a-f]{24}$/);
    expect(JSON.stringify(first)).not.toContain(input.accountId);
  });

  it("changes session identity across tenants and session keys", () => {
    const base = {
      universeId,
      provider: "telegram" as const,
      accountId: "primary",
      sessionKey: "family",
    };
    expect(channelSessionIdentity({ ...base, sessionKey: "work" }).sessionId).not.toBe(
      channelSessionIdentity(base).sessionId,
    );
    expect(
      channelSessionIdentity({
        ...base,
        universeId: "123e4567-e89b-42d3-a456-426614174000",
      }).sessionId,
    ).not.toBe(channelSessionIdentity(base).sessionId);
  });

  it("accepts Lightspeed's canonical default universe id", () => {
    const identity = channelSessionIdentity({
      universeId: defaultUniverseId,
      provider: "telegram",
      accountId: "primary",
      sessionKey: "family",
    });
    expect(identity.workflowId).toContain(`/${defaultUniverseId}/telegram/`);
    expect(lightspeedSessionWorkflowId(defaultUniverseId, identity.sessionId)).toBe(
      `${defaultUniverseId}/${identity.sessionId}`,
    );
  });

  it("composes the Lightspeed holder workflow id", () => {
    expect(lightspeedSessionWorkflowId(universeId, "channel:v1:telegram:test")).toBe(
      `${universeId}/channel:v1:telegram:test`,
    );
    expect(() => lightspeedSessionWorkflowId(universeId, "bad/session")).toThrow(
      /must not contain/,
    );
  });

  it("derives stable submission and terminal notification ids", () => {
    const first = channelTurnIdentity("lsbot.channels.v1/workflow", "42");
    expect(channelTurnIdentity("lsbot.channels.v1/workflow", "42")).toEqual(first);
    expect(first.submissionId).toMatch(/^channels-v1-[0-9a-f]{64}$/);
    expect(first.terminalToken).toMatch(/^channels-terminal-v1-[0-9a-f]{64}$/);
    expect(channelTurnIdentity("lsbot.channels.v1/workflow", "43")).not.toEqual(first);
  });

  it("derives an account-affine delivery queue without tenant context", () => {
    expect(channelDeliveryTaskQueue("telegram", "primary")).toBe(
      channelSessionIdentity({
        universeId,
        provider: "telegram",
        accountId: "primary",
        sessionKey: "family",
      }).deliveryTaskQueue,
    );
  });

  it("derives a stable pairing key without exposing chat identity", () => {
    const route = {
      provider: "whatsapp" as const,
      accountId: "primary",
      chatId: "family@g.us",
    };
    const key = channelPairingKey(route);
    expect(channelPairingKey(route)).toBe(key);
    expect(key).toMatch(/^channels-pairing-v1-[0-9a-f]{64}$/);
    expect(key).not.toContain(route.chatId);
  });
});
