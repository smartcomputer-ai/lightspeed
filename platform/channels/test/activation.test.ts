import { describe, expect, it } from "vitest";
import type { NormalizedInboundV1 } from "../src/contracts/channel.js";
import {
  classifyInbound,
  formatInboundEnvelope,
  resolveActivationSettings,
} from "../src/policy/activation.js";

const groupInbound: NormalizedInboundV1 = {
  version: 1,
  messageId: "42",
  route: { provider: "telegram", accountId: "primary", chatId: "-100123" },
  senderId: "7",
  senderName: "Lukas",
  timestampMs: 1_700_000_000_000,
  text: "ordinary group traffic",
  isDirect: false,
  mentionedBot: false,
  isReplyToBot: false,
};

describe("Channels activation policy", () => {
  it("forces direct sessions active while honoring bounded batching settings", () => {
    expect(
      resolveActivationSettings("direct", {
        group: "silent",
        batching: { debounceMs: 0, maxWaitMs: 10, maxMessages: 1 },
      }),
    ).toEqual({
      mode: "dm",
      triggerPrefixes: ["/ask", "/lightspeed"],
      mentionNames: [],
      batching: { debounceMs: 0, maxWaitMs: 10, maxMessages: 1 },
      roomContextLimit: 200,
    });
  });

  it("keeps ambient group traffic as context and activates native mentions", () => {
    const settings = resolveActivationSettings("group", {
      group: "mention",
      mentionNames: ["lightspeed"],
    });
    expect(classifyInbound(groupInbound, settings)).toEqual({
      kind: "roomEvent",
      text: "ordinary group traffic",
    });
    expect(
      classifyInbound(
        { ...groupInbound, text: "@lightspeed, help please", mentionedBot: true },
        settings,
      ),
    ).toEqual({ kind: "userTurn", text: "help please" });
  });

  it("allows explicit prefixes even in silent groups", () => {
    const settings = resolveActivationSettings("group", {
      group: "silent",
      triggerPrefixes: ["/ask"],
    });
    expect(classifyInbound({ ...groupInbound, text: "/ask investigate" }, settings)).toEqual({
      kind: "userTurn",
      text: "investigate",
    });
    expect(classifyInbound({ ...groupInbound, text: "/ask" }, settings)).toEqual({
      kind: "drop",
      reason: "empty-trigger",
    });
  });

  it("renders stable channel envelopes for context and batches", () => {
    expect(formatInboundEnvelope(groupInbound)).toBe(
      "[telegram:group -100123 #42] Lukas (2023-11-14 22:13Z): ordinary group traffic",
    );
  });

  it("rejects malformed durable activation configuration", () => {
    expect(() => resolveActivationSettings("group", { group: "sometimes" })).toThrow(
      "activation.group",
    );
    expect(() =>
      resolveActivationSettings("group", { batching: { maxMessages: 100 } }),
    ).toThrow("between 1 and 32");
  });
});
