import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { BindingAccessCandidate } from "../src/config.js";
import type { LightspeedReply, LightspeedRoomEvent, LightspeedSessionBridge, LightspeedTurn } from "../src/lightspeed.js";
import { MessagingBridgeRuntime, type ChannelPolicy, type NormalizedInbound } from "../src/runtime.js";
import { JsonBridgeStore } from "../src/store.js";

interface LightspeedCall {
  kind: "turn" | "room";
  sessionId: string;
  texts: string[];
  mediaMimes?: string[];
}

class FakeLightspeed {
  readonly calls: LightspeedCall[] = [];
  failTurns = false;
  turnError: unknown = null;
  messagingToolUsed = false;

  async appendRoomEvents(sessionId: string, events: readonly LightspeedRoomEvent[]): Promise<void> {
    this.calls.push({ kind: "room", sessionId, texts: events.map((event) => event.text) });
  }

  async submitTurn(turn: LightspeedTurn): Promise<LightspeedReply> {
    if (this.failTurns) {
      throw this.turnError ?? new Error("lightspeed unavailable");
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
      messagingToolUsed: this.messagingToolUsed,
    };
  }
}

const policy: ChannelPolicy = {
  triggerPrefixes: ["/ask"],
  mentionNames: [],
  botUsername: "lightspeed_bot",
  groupActivation: "mention",
};

let dir: string;
let store: JsonBridgeStore;
let lightspeed: FakeLightspeed;
let runtime: MessagingBridgeRuntime;
let replies: string[];

function makeRuntime(): MessagingBridgeRuntime {
  return new MessagingBridgeRuntime({
    lightspeed: lightspeed as unknown as LightspeedSessionBridge,
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
  lightspeed = new FakeLightspeed();
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
    turnAllowed: true,
    controlAllowed: false,
    ...overrides,
  };
}

const io = () => ({
  sendReply: async (text: string) => {
    replies.push(text);
  },
});

const pairableBinding = (overrides: Partial<BindingAccessCandidate> = {}): BindingAccessCandidate => ({
  bindingId: "lukas-telegram",
  pairing: { code: "PAIRME" },
  profile: { kind: "named", profileId: "personal" },
  profileLabel: "personal",
  sessionKey: "lukas",
  ...overrides,
});

describe("MessagingBridgeRuntime", () => {
  it("batches a burst of direct messages into one run", async () => {
    const dm = { isDirect: true, conversationKey: "telegram:dm-1", chatId: "dm-1" };
    await runtime.handleInbound(inbound({ ...dm, text: "first" }), policy, io());
    await runtime.handleInbound(inbound({ ...dm, text: "second" }), policy, io());
    await runtime.handleInbound(inbound({ ...dm, text: "third" }), policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    const turns = lightspeed.calls.filter((call) => call.kind === "turn");
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
      inbound({ text: "@lightspeed_bot summarize", mentionedBot: true }),
      policy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(lightspeed.calls.map((call) => call.kind)).toEqual(["room", "turn"]);
    const room = lightspeed.calls[0];
    expect(room?.texts.join("\n")).toContain("chatter one");
    expect(room?.texts.join("\n")).toContain("chatter two");
    const turn = lightspeed.calls[1];
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

    const turns = lightspeed.calls.filter((call) => call.kind === "turn");
    expect(turns).toHaveLength(1);
    // DMs carry the envelope too: the #id markers make react/edit/reply_to
    // targetable.
    expect(turns[0]?.texts[0]).toContain("once");
    expect(turns[0]?.texts[0]).toContain("#dup-1");
  });

  it("persists /activation changes and applies them to later messages", async () => {
    await runtime.handleInbound(
      inbound({ text: "/activation always", controlAllowed: true }),
      policy,
      io(),
    );
    expect(replies[0]).toContain("always");

    // A second runtime over the same store sees the persisted activation.
    const second = makeRuntime();
    await second.handleInbound(inbound({ text: "no mention needed" }), policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await second.flush();

    const turns = lightspeed.calls.filter((call) => call.kind === "turn");
    expect(turns).toHaveLength(1);
    expect(turns[0]?.texts[0]).toContain("no mention needed");
  });

  it("binds two conversations sharing a session key to one session", async () => {
    const a = {
      isDirect: true,
      conversationKey: "telegram:a",
      conversationParts: ["telegram", "default", "a"],
      chatId: "a",
      sessionKey: "team",
    };
    const b = {
      isDirect: true,
      conversationKey: "telegram:b",
      conversationParts: ["telegram", "default", "b"],
      chatId: "b",
      sessionKey: "team",
    };
    await runtime.handleInbound(inbound({ ...a, text: "from a" }), policy, io());
    await runtime.handleInbound(inbound({ ...b, text: "from b" }), policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    const turns = lightspeed.calls.filter((call) => call.kind === "turn");
    expect(turns).toHaveLength(2);
    expect(turns[0]?.sessionId).toBe(turns[1]?.sessionId);
  });

  it("gives conversations without a session key their own sessions", async () => {
    const a = {
      isDirect: true,
      conversationKey: "telegram:a",
      conversationParts: ["telegram", "default", "a"],
      chatId: "a",
    };
    const b = {
      isDirect: true,
      conversationKey: "telegram:b",
      conversationParts: ["telegram", "default", "b"],
      chatId: "b",
    };
    await runtime.handleInbound(inbound({ ...a, text: "from a" }), policy, io());
    await runtime.handleInbound(inbound({ ...b, text: "from b" }), policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    const turns = lightspeed.calls.filter((call) => call.kind === "turn");
    expect(turns).toHaveLength(2);
    expect(turns[0]?.sessionId).not.toBe(turns[1]?.sessionId);
  });

  it("replies with an authorization error and starts no run for a denied direct sender", async () => {
    await runtime.handleInbound(
      inbound({ isDirect: true, text: "let me in", turnAllowed: false }),
      policy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(lightspeed.calls.filter((call) => call.kind === "turn")).toHaveLength(0);
    expect(replies).toHaveLength(1);
    expect(replies[0]).toContain("not authorized");
  });

  it("pairs a direct chat by exact code before forwarding later turns", async () => {
    const dm = {
      isDirect: true,
      conversationKey: "telegram:dm-pairing",
      pairingKey: "telegram:pair-dm",
      chatId: "dm-1",
      turnAllowed: false,
      bindingCandidates: [pairableBinding()],
    };

    await runtime.handleInbound(inbound({ ...dm, text: "hello", messageId: "before" }), policy, io());
    await runtime.flush();
    expect(replies.at(-1)).toContain("not paired");
    expect(lightspeed.calls.filter((call) => call.kind === "turn")).toHaveLength(0);

    await runtime.handleInbound(inbound({ ...dm, text: "PAIRME", messageId: "pair" }), policy, io());
    await runtime.flush();
    expect(replies.at(-1)).toContain("Paired");
    expect(lightspeed.calls.filter((call) => call.kind === "turn")).toHaveLength(0);

    await runtime.handleInbound(inbound({ ...dm, text: "after", messageId: "after" }), policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    const turns = lightspeed.calls.filter((call) => call.kind === "turn");
    expect(turns).toHaveLength(1);
    expect(turns[0]?.texts[0]).toContain("after");
  });

  it("pairs a whole group and then allows any group sender through that binding", async () => {
    const group = {
      isDirect: false,
      conversationKey: "telegram:group-pairing-thread",
      pairingKey: "telegram:pair-group",
      chatId: "group-1",
      turnAllowed: false,
      bindingCandidates: [pairableBinding()],
    };

    await runtime.handleInbound(inbound({ ...group, text: "ambient" }), policy, io());
    await runtime.flush();
    expect(replies).toHaveLength(0);
    expect(lightspeed.calls).toHaveLength(0);

    await runtime.handleInbound(
      inbound({ ...group, text: "@lightspeed_bot help", mentionedBot: true, messageId: "prompt" }),
      policy,
      io(),
    );
    await runtime.flush();
    expect(replies.at(-1)).toContain("not paired");
    expect(lightspeed.calls).toHaveLength(0);

    await runtime.handleInbound(inbound({ ...group, text: "PAIRME", messageId: "pair-group" }), policy, io());
    await runtime.flush();
    expect(replies.at(-1)).toContain("Paired");

    await runtime.handleInbound(
      inbound({
        ...group,
        text: "@lightspeed_bot status?",
        mentionedBot: true,
        messageId: "after-group",
        senderId: "user-2",
        senderName: "Bob",
        turnAllowed: false,
      }),
      policy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    const turns = lightspeed.calls.filter((call) => call.kind === "turn");
    expect(turns).toHaveLength(1);
    expect(turns[0]?.texts[0]).toContain("Bob");
    expect(turns[0]?.texts[0]).toContain("status?");
  });

  it("drops a denied group sender silently", async () => {
    await runtime.handleInbound(
      inbound({ text: "@lightspeed_bot help", mentionedBot: true, turnAllowed: false }),
      policy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(lightspeed.calls.filter((call) => call.kind === "turn")).toHaveLength(0);
    expect(replies).toHaveLength(0);
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

    const turns = lightspeed.calls.filter((call) => call.kind === "turn");
    expect(turns).toHaveLength(1);
    expect(turns[0]?.mediaMimes).toEqual(["image/jpeg"]);
    expect(downloads).toBe(1);
  });

  it("fires the typing indicator while a turn is running", async () => {
    let typingCalls = 0;
    await runtime.handleInbound(inbound({ isDirect: true, text: "hello" }), policy, {
      ...io(),
      setTyping: async () => {
        typingCalls += 1;
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(typingCalls).toBeGreaterThanOrEqual(1);
    expect(replies).toHaveLength(1);
  });

  it("clears the typing indicator when a turn completes silently", async () => {
    const typingEvents: string[] = [];
    lightspeed.messagingToolUsed = true;
    await runtime.handleInbound(inbound({ isDirect: true, text: "quiet ack" }), policy, {
      ...io(),
      setTyping: async () => {
        typingEvents.push("set");
      },
      clearTyping: async () => {
        typingEvents.push("clear");
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(typingEvents[0]).toBe("set");
    expect(typingEvents.at(-1)).toBe("clear");
    expect(replies).toHaveLength(0);
  });

  it("suppresses final-text delivery when the run used messaging tools", async () => {
    lightspeed.messagingToolUsed = true;
    await runtime.handleInbound(
      inbound({ isDirect: true, text: "send it via the tool" }),
      policy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(lightspeed.calls.filter((call) => call.kind === "turn")).toHaveLength(1);
    expect(replies).toHaveLength(0);
  });

  it("reports run failures back to the chat and records the error", async () => {
    lightspeed.failTurns = true;
    const message = inbound({ isDirect: true, text: "boom" });
    await runtime.handleInbound(message, policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(replies[0]).toContain("Lightspeed could not answer");
    // The message is marked done (not retried forever).
    expect(await store.beginMessage(message.messageKey)).toBe("duplicate");
  });

  it("surfaces audio transcription admission failures as audio failures", async () => {
    const error = new Error("run rejected") as Error & {
      data: { kind: string; message: string };
    };
    error.data = {
      kind: "transcription_failure",
      message: "OpenAI could not transcribe the audio",
    };
    lightspeed.failTurns = true;
    lightspeed.turnError = error;

    const message = inbound({ isDirect: true, text: "(sent a voice note)" });
    await runtime.handleInbound(message, policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(replies[0]).toContain("could not transcribe this audio message");
    expect(replies[0]).toContain("OpenAI could not transcribe the audio");
    expect(await store.beginMessage(message.messageKey)).toBe("duplicate");
  });
});
