import { describe, expect, it } from "vitest";
import {
  classifyInbound,
  formatEnvelope,
  parseControlCommand,
  shouldQuoteChunk,
  type ClassifyInput,
  type ClassifyOptions,
} from "../src/policy.js";

const baseMessage: ClassifyInput = {
  text: "hello there",
  isDirect: false,
  isFromSelf: false,
  mentionedBot: false,
  isReplyToBot: false,
  senderAllowed: false,
};

const baseOptions: ClassifyOptions = {
  activation: "mention",
  triggerPrefixes: ["/ask", "/forge"],
  mentionNames: ["forge"],
  botUsername: "forge_bot",
};

describe("classifyInbound", () => {
  it("drops messages from the bot itself", () => {
    expect(
      classifyInbound({ ...baseMessage, isFromSelf: true }, baseOptions),
    ).toEqual({ kind: "drop", reason: "self" });
  });

  it("treats direct messages as user turns without any trigger", () => {
    expect(
      classifyInbound({ ...baseMessage, isDirect: true }, { ...baseOptions, activation: "dm" }),
    ).toEqual({ kind: "userTurn", text: "hello there" });
  });

  it("activates on a native mention in mention mode and strips it", () => {
    expect(
      classifyInbound(
        { ...baseMessage, text: "@forge_bot what is up", mentionedBot: true },
        baseOptions,
      ),
    ).toEqual({ kind: "userTurn", text: "what is up" });
  });

  it("activates on a reply to the bot in mention mode", () => {
    expect(classifyInbound({ ...baseMessage, isReplyToBot: true }, baseOptions)).toEqual({
      kind: "userTurn",
      text: "hello there",
    });
  });

  it("buffers plain group chatter as a room event in mention mode", () => {
    expect(classifyInbound(baseMessage, baseOptions)).toEqual({
      kind: "roomEvent",
      text: "hello there",
    });
  });

  it("turns every group message into a user turn in always mode", () => {
    expect(
      classifyInbound(baseMessage, { ...baseOptions, activation: "always" }),
    ).toEqual({ kind: "userTurn", text: "hello there" });
  });

  it("keeps silent chats listen-only except for trigger prefixes", () => {
    expect(
      classifyInbound(
        { ...baseMessage, mentionedBot: true },
        { ...baseOptions, activation: "silent" },
      ),
    ).toEqual({ kind: "roomEvent", text: "hello there" });
    expect(
      classifyInbound(
        { ...baseMessage, text: "/ask are you there" },
        { ...baseOptions, activation: "silent" },
      ),
    ).toEqual({ kind: "userTurn", text: "are you there" });
  });

  it("routes control commands only for allowed senders", () => {
    expect(
      classifyInbound({ ...baseMessage, text: "/activation always", senderAllowed: true }, baseOptions),
    ).toEqual({ kind: "control", command: { kind: "activation", mode: "always" } });
    expect(
      classifyInbound({ ...baseMessage, text: "/activation always" }, baseOptions),
    ).toEqual({ kind: "roomEvent", text: "/activation always" });
  });

  it("drops empty messages", () => {
    expect(classifyInbound({ ...baseMessage, text: "   " }, baseOptions)).toEqual({
      kind: "drop",
      reason: "empty",
    });
  });
});

describe("parseControlCommand", () => {
  it("parses activation modes including bot-suffixed commands", () => {
    expect(parseControlCommand("/activation mention")).toEqual({
      kind: "activation",
      mode: "mention",
    });
    expect(parseControlCommand("/activation@forge_bot silent")).toEqual({
      kind: "activation",
      mode: "silent",
    });
  });

  it("parses /new and /status", () => {
    expect(parseControlCommand("/new")).toEqual({ kind: "new" });
    expect(parseControlCommand("/status")).toEqual({ kind: "status" });
  });

  it("returns null for ordinary text", () => {
    expect(parseControlCommand("what is /status of the build")).toBeNull();
  });
});

describe("shouldQuoteChunk", () => {
  it("never quotes in direct chats", () => {
    expect(shouldQuoteChunk("first", true, 0)).toBe(false);
    expect(shouldQuoteChunk("all", true, 0)).toBe(false);
  });

  it("quotes only the first chunk in group first mode", () => {
    expect(shouldQuoteChunk("first", false, 0)).toBe(true);
    expect(shouldQuoteChunk("first", false, 1)).toBe(false);
  });

  it("quotes every chunk in all mode and none in off mode", () => {
    expect(shouldQuoteChunk("all", false, 2)).toBe(true);
    expect(shouldQuoteChunk("off", false, 0)).toBe(false);
  });
});

describe("formatEnvelope", () => {
  it("renders group envelopes with sender and timestamp", () => {
    expect(
      formatEnvelope({
        provider: "telegram",
        chatLabel: "Engineering",
        isDirect: false,
        senderName: "Alice",
        timestampMs: Date.UTC(2026, 5, 12, 12, 1),
        text: "the deploy looks stuck",
      }),
    ).toBe("[telegram:group Engineering] Alice (2026-06-12 12:01Z): the deploy looks stuck");
  });

  it("includes the channel message id when provided", () => {
    expect(
      formatEnvelope({
        provider: "telegram",
        chatLabel: "Engineering",
        isDirect: false,
        senderName: "Alice",
        timestampMs: Date.UTC(2026, 5, 12, 12, 1),
        text: "ping",
        messageId: "4123",
      }),
    ).toBe("[telegram:group Engineering #4123] Alice (2026-06-12 12:01Z): ping");
  });

  it("renders dm envelopes", () => {
    expect(
      formatEnvelope({
        provider: "whatsapp",
        chatLabel: "dm",
        isDirect: true,
        senderName: "Lukas",
        timestampMs: Date.UTC(2026, 5, 12, 7, 30),
        text: "ping",
      }),
    ).toBe("[whatsapp:dm] Lukas (2026-06-12 07:30Z): ping");
  });
});
