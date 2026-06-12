import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { ForgeReply, ForgeRoomEvent, ForgeSessionBridge, ForgeTurn } from "../src/forge.js";
import { MessagingBridgeRuntime, type ChannelPolicy, type NormalizedInbound } from "../src/runtime.js";
import { JsonBridgeStore } from "../src/store.js";

interface ForgeCall {
  kind: "turn" | "room";
  sessionId: string;
  texts: string[];
  mediaMimes?: string[];
}

class FakeForge {
  readonly calls: ForgeCall[] = [];
  failTurns = false;

  async appendRoomEvents(sessionId: string, events: readonly ForgeRoomEvent[]): Promise<void> {
    this.calls.push({ kind: "room", sessionId, texts: events.map((event) => event.text) });
  }

  async submitTurn(turn: ForgeTurn): Promise<ForgeReply> {
    if (this.failTurns) {
      throw new Error("forge unavailable");
    }
    this.calls.push({
      kind: "turn",
      sessionId: turn.sessionId,
      texts: [turn.text],
      mediaMimes: (turn.media ?? []).map((item) => item.mime),
    });
    return {
      cursor: null,
      runId: "run_1",
      sessionId: turn.sessionId,
      text: `echo: ${turn.text}`,
    };
  }
}

const policy: ChannelPolicy = {
  triggerPrefixes: ["/ask"],
  mentionNames: [],
  botUsername: "forge_bot",
  groupActivation: "mention",
};

let dir: string;
let store: JsonBridgeStore;
let forge: FakeForge;
let runtime: MessagingBridgeRuntime;
let replies: string[];

function makeRuntime(): MessagingBridgeRuntime {
  return new MessagingBridgeRuntime({
    forge: forge as unknown as ForgeSessionBridge,
    store,
    sessionPrefix: "test",
    log: () => undefined,
    runtime: {
      debounceMs: 20,
      turnMaxBatch: 10,
      turnMaxWaitMs: 200,
      roomFlushMs: 60_000,
      roomFlushMax: 50,
      roomBudget: 50,
    },
  });
}

beforeEach(async () => {
  dir = await mkdtemp(path.join(tmpdir(), "bridge-test-"));
  store = new JsonBridgeStore(path.join(dir, "state.json"));
  forge = new FakeForge();
  runtime = makeRuntime();
  replies = [];
});

afterEach(async () => {
  await rm(dir, { recursive: true, force: true });
});

let messageCounter = 0;

function inbound(overrides: Partial<NormalizedInbound>): NormalizedInbound {
  messageCounter += 1;
  const messageId = overrides.messageId ?? `m${messageCounter}`;
  return {
    provider: "telegram",
    accountId: "default",
    chatId: "chat-1",
    conversationKey: "telegram:conv-1",
    conversationParts: ["telegram", "default", "chat-1", "main"],
    messageId,
    messageKey: `telegram:${messageId}`,
    senderId: "user-1",
    senderName: "Alice",
    timestampMs: Date.UTC(2026, 5, 12, 12, 0),
    text: "hello",
    isDirect: false,
    chatLabel: "Engineering",
    mentionedBot: false,
    isReplyToBot: false,
    isFromSelf: false,
    senderAllowed: false,
    ...overrides,
  };
}

const io = () => ({
  sendReply: async (text: string) => {
    replies.push(text);
  },
});

describe("MessagingBridgeRuntime", () => {
  it("batches a burst of direct messages into one run", async () => {
    const dm = { isDirect: true, conversationKey: "telegram:dm-1", chatId: "dm-1" };
    await runtime.handleInbound(inbound({ ...dm, text: "first" }), policy, io());
    await runtime.handleInbound(inbound({ ...dm, text: "second" }), policy, io());
    await runtime.handleInbound(inbound({ ...dm, text: "third" }), policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    const turns = forge.calls.filter((call) => call.kind === "turn");
    expect(turns).toHaveLength(1);
    const text = turns[0]?.texts[0] ?? "";
    expect(text).toContain("first");
    expect(text).toContain("second");
    expect(text).toContain("third");
    expect(replies).toHaveLength(1);
  });

  it("buffers group chatter and appends it before an activating mention", async () => {
    await runtime.handleInbound(inbound({ text: "chatter one" }), policy, io());
    await runtime.handleInbound(inbound({ text: "chatter two" }), policy, io());
    await runtime.handleInbound(
      inbound({ text: "@forge_bot summarize", mentionedBot: true }),
      policy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(forge.calls.map((call) => call.kind)).toEqual(["room", "turn"]);
    const room = forge.calls[0];
    expect(room?.texts.join("\n")).toContain("chatter one");
    expect(room?.texts.join("\n")).toContain("chatter two");
    const turn = forge.calls[1];
    expect(turn?.texts[0]).toContain("summarize");
    expect(turn?.sessionId).toBe(room?.sessionId);
  });

  it("ignores its own messages and duplicate deliveries", async () => {
    await runtime.handleInbound(
      inbound({ isFromSelf: true, isDirect: true, text: "echo" }),
      policy,
      io(),
    );
    const duplicate = inbound({ isDirect: true, text: "once", messageId: "dup-1" });
    await runtime.handleInbound(duplicate, policy, io());
    await runtime.handleInbound(duplicate, policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    const turns = forge.calls.filter((call) => call.kind === "turn");
    expect(turns).toHaveLength(1);
    expect(turns[0]?.texts[0]).toBe("once");
  });

  it("persists /activation changes and applies them to later messages", async () => {
    await runtime.handleInbound(
      inbound({ text: "/activation always", senderAllowed: true }),
      policy,
      io(),
    );
    expect(replies[0]).toContain("always");

    // A second runtime over the same store sees the persisted activation.
    const second = makeRuntime();
    await second.handleInbound(inbound({ text: "no mention needed" }), policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await second.flush();

    const turns = forge.calls.filter((call) => call.kind === "turn");
    expect(turns).toHaveLength(1);
    expect(turns[0]?.texts[0]).toContain("no mention needed");
  });

  it("rebinds to a fresh session on /new", async () => {
    await runtime.handleInbound(
      inbound({ isDirect: true, text: "before" }),
      policy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    await runtime.handleInbound(
      inbound({ isDirect: true, text: "/new", senderAllowed: true }),
      policy,
      io(),
    );
    await runtime.handleInbound(inbound({ isDirect: true, text: "after" }), policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    const turns = forge.calls.filter((call) => call.kind === "turn");
    expect(turns).toHaveLength(2);
    expect(turns[0]?.sessionId).not.toBe(turns[1]?.sessionId);
  });

  it("downloads media only for user turns and passes it to the run", async () => {
    let downloads = 0;
    const photo = inbound({
      isDirect: true,
      text: "(sent an image)",
      fetchMedia: async () => {
        downloads += 1;
        return [{ base64: "aGk=", mime: "image/jpeg", name: "photo.jpg" }];
      },
    });
    await runtime.handleInbound(photo, policy, io());

    // Group chatter with media is buffered as a room event without download.
    const roomPhoto = inbound({
      text: "(sent an image)",
      conversationKey: "telegram:conv-room",
      chatId: "chat-room",
      fetchMedia: async () => {
        downloads += 1;
        return [{ base64: "aGk=", mime: "image/jpeg" }];
      },
    });
    await runtime.handleInbound(roomPhoto, policy, io());

    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    const turns = forge.calls.filter((call) => call.kind === "turn");
    expect(turns).toHaveLength(1);
    expect(turns[0]?.mediaMimes).toEqual(["image/jpeg"]);
    expect(downloads).toBe(1);
  });

  it("reports run failures back to the chat and records the error", async () => {
    forge.failTurns = true;
    const message = inbound({ isDirect: true, text: "boom" });
    await runtime.handleInbound(message, policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(replies[0]).toContain("Forge could not answer");
    // The message is marked done (not retried forever).
    expect(await store.beginMessage(message.messageKey)).toBe("duplicate");
  });
});
