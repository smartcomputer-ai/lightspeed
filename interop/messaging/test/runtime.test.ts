import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { BindingAccessCandidate } from "../src/config.js";
import type { LightspeedReply, LightspeedSessionBridge, LightspeedTurn } from "../src/lightspeed.js";
import { MessagingBridgeRuntime, type ChannelPolicy, type NormalizedInbound } from "../src/runtime.js";
import { JsonBridgeStore } from "../src/store.js";

interface LightspeedCall {
  kind: "turn" | "context" | "contextTurn" | "contextRemove";
  sessionId: string;
  texts: string[];
  mediaMimes?: string[];
  contextKeys?: string[];
  removedKeys?: string[];
}

class FakeLightspeed {
  readonly calls: LightspeedCall[] = [];
  failTurns = false;
  failAppend = false;
  appendError: unknown = null;
  appendFailures: { key: string; kind: string; message: string }[] = [];
  turnError: unknown = null;
  messagingToolUsed = false;
  /// Activation text the fake derives for appended audio media entries.
  audioTranscript = "/ask transcribed voice request";
  /// When set, replaces the activation text echoed for the text entry.
  textActivationOverride: string | null = null;
  /// When true, context/remove calls throw after being recorded.
  failRemove = false;
  /// Keys context/remove reports as per-key failed.
  removeFailedKeys = new Set<string>();

  async appendMessageContext(message: {
    sessionId: string;
    key: string;
    text: string;
    media?: readonly { mime: string; name?: string }[];
  }) {
    if (this.failAppend) {
      throw this.appendError ?? new Error("append unavailable");
    }
    const contextKeys = [
      ...(message.text ? [`${message.key}.text`] : []),
      ...(message.media ?? []).map((_, index) => `${message.key}.media.${index}`),
    ];
    this.calls.push({
      kind: "context",
      sessionId: message.sessionId,
      texts: message.text ? [message.text] : [],
      mediaMimes: (message.media ?? []).map((item) => item.mime),
      contextKeys,
    });
    if (this.appendFailures.length > 0) {
      return {
        keys: [],
        activationText: [],
        failures: this.appendFailures,
      };
    }
    return {
      keys: contextKeys,
      activationText: [
        ...(message.text
          ? [{ key: `${message.key}.text`, text: this.textActivationOverride ?? message.text }]
          : []),
        ...(message.media ?? []).flatMap((item, index) =>
          item.mime.startsWith("audio/")
            ? [{ key: `${message.key}.media.${index}`, text: this.audioTranscript }]
            : [],
        ),
      ],
      failures: [],
    };
  }

  async removeContext(sessionId: string, keys: readonly string[]) {
    this.calls.push({
      kind: "contextRemove",
      sessionId,
      texts: [],
      removedKeys: [...keys],
    });
    if (this.failRemove) {
      throw new Error("remove unavailable");
    }
    return keys.map((key) =>
      this.removeFailedKeys.has(key)
        ? {
            key,
            status: "failed" as const,
            failure: { kind: "invariantViolation", message: "cannot remove" },
          }
        : { key, status: "removed" as const, failure: null },
    );
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

  async submitContextTurn(turn: {
    sessionId: string;
    contextKeys: readonly string[];
  }): Promise<LightspeedReply> {
    if (this.failTurns) {
      throw this.turnError ?? new Error("lightspeed unavailable");
    }
    this.calls.push({
      kind: "contextTurn",
      sessionId: turn.sessionId,
      texts: [],
      contextKeys: [...turn.contextKeys],
    });
    return {
      cursor: null,
      runId: "run_1",
      sessionId: turn.sessionId,
      text: `echo: ${turn.contextKeys.join(",")}`,
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

function makeRuntime(retention: { high?: number; low?: number } = {}): MessagingBridgeRuntime {
  return new MessagingBridgeRuntime({
    lightspeed: lightspeed as unknown as LightspeedSessionBridge,
    store,
    sessionPrefix: "test",
    log: () => undefined,
    runtime: {
      debounceMs: 20,
      turnMaxBatch: 10,
      turnMaxWaitMs: 200,
      // Retention is disabled by default so unrelated tests never prune.
      roomRetentionHigh: retention.high ?? 0,
      roomRetentionLow: retention.low ?? 0,
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

  it("appends group chatter eagerly before an activating mention runs", async () => {
    await runtime.handleInbound(inbound({ text: "chatter one" }), policy, io());
    await runtime.handleInbound(inbound({ text: "chatter two" }), policy, io());
    await runtime.handleInbound(
      inbound({ text: "@lightspeed_bot summarize", mentionedBot: true }),
      policy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    // Each room message is committed via context/append at receipt, before
    // the mention's run starts.
    expect(lightspeed.calls.map((call) => call.kind)).toEqual([
      "context",
      "context",
      "context",
      "contextTurn",
    ]);
    expect(lightspeed.calls[0]?.texts[0]).toContain("chatter one");
    expect(lightspeed.calls[1]?.texts[0]).toContain("chatter two");
    const mention = lightspeed.calls[2];
    expect(mention?.texts[0]).toContain("summarize");
    const turn = lightspeed.calls[3];
    // The run starts from the mention's own context keys; chatter is already
    // part of the session context.
    expect(turn?.contextKeys).toEqual(mention?.contextKeys);
    expect(turn?.sessionId).toBe(lightspeed.calls[0]?.sessionId);
  });

  it("appends an unaddressed text room message immediately without starting a run in mention mode", async () => {
    const chatter = inbound({ text: "just chatting", messageId: "room-1" });
    await runtime.handleInbound(chatter, policy, io());
    // Channel redelivery of the same message does not re-append.
    await runtime.handleInbound(chatter, policy, io());
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(lightspeed.calls.map((call) => call.kind)).toEqual(["context"]);
    expect(lightspeed.calls[0]?.texts[0]).toContain("just chatting");
    expect(replies).toHaveLength(0);
  });

  it("appends text room messages in silent mode without starting a run", async () => {
    await runtime.handleInbound(
      inbound({ text: "/activation silent", controlAllowed: true }),
      policy,
      io(),
    );
    expect(replies[0]).toContain("silent");

    await runtime.handleInbound(
      inbound({ text: "what do you think, @lightspeed_bot?", mentionedBot: true }),
      policy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    // Even a mention stays a room event in silent mode (only explicit trigger
    // prefixes escape): appended as context, no run, no reply beyond the
    // control acknowledgement.
    expect(lightspeed.calls.map((call) => call.kind)).toEqual(["context"]);
    expect(lightspeed.calls[0]?.texts[0]).toContain("what do you think");
    expect(replies).toHaveLength(1);
  });

  const mentionPolicy: ChannelPolicy = {
    triggerPrefixes: ["/ask"],
    mentionNames: ["lightspeed"],
    botUsername: "lightspeed_bot",
    groupActivation: "mention",
  };

  it("does not activate on text-derived activation text or envelope mention names", async () => {
    // The chat label and message text both contain the mention name, and the
    // server-side echo of the text entry even carries an explicit trigger.
    // The classifier already saw the raw text; only media-derived activation
    // text may trigger, so this room message must not start a run.
    lightspeed.textActivationOverride = "@lightspeed pretend trigger from text entry";
    await runtime.handleInbound(
      inbound({
        text: "the lightspeed rollout looks fine",
        chatLabel: "lightspeed fans",
      }),
      mentionPolicy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(lightspeed.calls.map((call) => call.kind)).toEqual(["context"]);
    expect(replies).toHaveLength(0);
  });

  it("activates when a voice transcript contains a mention name", async () => {
    lightspeed.audioTranscript = "[audio transcript: voice.ogg]\n@lightspeed what is the deploy status?";
    await runtime.handleInbound(
      inbound({
        text: "",
        chatLabel: "Engineering",
        fetchMedia: async () => [{ base64: "aGk=", mime: "audio/ogg", name: "voice.ogg" }],
      }),
      mentionPolicy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    const context = lightspeed.calls.find((call) => call.kind === "context");
    const contextTurn = lightspeed.calls.find((call) => call.kind === "contextTurn");
    expect(contextTurn?.contextKeys).toEqual(context?.contextKeys);
    expect(replies).toHaveLength(1);
  });

  it("does not resubmit context-ingested turns as input in a mixed batch", async () => {
    // A mention-mode turn is ingested via context/append, then /activation
    // flips the group to always mid-debounce, so the follow-up lands in the
    // same batch as a plain input turn.
    await runtime.handleInbound(
      inbound({
        text: "@lightspeed_bot summarize the thread",
        mentionedBot: true,
        messageId: "ctx-1",
        fetchMedia: async () => [{ base64: "aGk=", mime: "image/jpeg", name: "photo.jpg" }],
      }),
      policy,
      io(),
    );
    await runtime.handleInbound(
      inbound({ text: "/activation always", controlAllowed: true, messageId: "flip" }),
      policy,
      io(),
    );
    await runtime.handleInbound(
      inbound({ text: "plain follow-up", messageId: "plain-1" }),
      policy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    // The ingested turn runs from context (source=context) and the plain turn
    // from input (source=input); neither leaks into the other.
    expect(lightspeed.calls.map((call) => call.kind)).toEqual(["context", "contextTurn", "turn"]);
    const context = lightspeed.calls[0];
    const contextTurn = lightspeed.calls[1];
    expect(contextTurn?.contextKeys).toEqual(context?.contextKeys);
    const turn = lightspeed.calls[2];
    expect(turn?.texts[0]).toContain("plain follow-up");
    expect(turn?.texts[0]).not.toContain("summarize the thread");
    expect(turn?.mediaMimes).toEqual([]);
    // Both groups replied and both messages are marked done.
    expect(replies).toHaveLength(3);
    expect(await store.beginMessage("telegram:ctx-1")).toBe("duplicate");
    expect(await store.beginMessage("telegram:plain-1")).toBe("duplicate");
  });

  it("marks mention-mode append exceptions retryable", async () => {
    lightspeed.failAppend = true;
    const message = inbound({
      text: "@lightspeed_bot summarize",
      mentionedBot: true,
      messageId: "append-fail",
    });

    await runtime.handleInbound(message, policy, io());
    await runtime.flush();

    expect(lightspeed.calls.filter((call) => call.kind === "contextTurn")).toHaveLength(0);
    expect(await store.beginMessage(message.messageKey)).toBe("new");
  });

  it("marks admission-rejected append results retryable", async () => {
    lightspeed.appendFailures = [
      {
        key: "channel.room.test.text",
        kind: "admissionRejected",
        message: "context append was rejected",
      },
    ];
    const message = inbound({
      text: "@lightspeed_bot summarize",
      mentionedBot: true,
      messageId: "append-rejected",
    });

    await runtime.handleInbound(message, policy, io());
    await runtime.flush();

    expect(lightspeed.calls.filter((call) => call.kind === "contextTurn")).toHaveLength(0);
    expect(await store.beginMessage(message.messageKey)).toBe("new");
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
    expect(turns).toHaveLength(0);
    const contextTurns = lightspeed.calls.filter((call) => call.kind === "contextTurn");
    expect(contextTurns).toHaveLength(1);
    const context = lightspeed.calls.find((call) => call.kind === "context");
    expect(context?.texts[0]).toContain("Bob");
    expect(context?.texts[0]).toContain("status?");
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

  it("downloads direct-turn media and appends allowed group room media", async () => {
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

    // Allowed group chatter with media is downloaded and appended even when it
    // does not trigger.
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
    const contexts = lightspeed.calls.filter((call) => call.kind === "context");
    expect(contexts).toHaveLength(1);
    expect(contexts[0]?.mediaMimes).toEqual(["image/jpeg"]);
    expect(downloads).toBe(2);
  });

  it("starts a mention-mode run when appended voice transcript contains a trigger", async () => {
    let downloads = 0;
    await runtime.handleInbound(
      inbound({
        text: "",
        fetchMedia: async () => {
          downloads += 1;
          return [{ base64: "aGk=", mime: "audio/ogg", name: "voice.ogg" }];
        },
      }),
      policy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    expect(downloads).toBe(1);
    const context = lightspeed.calls.find((call) => call.kind === "context");
    expect(context?.mediaMimes).toEqual(["audio/ogg"]);
    const contextTurn = lightspeed.calls.find((call) => call.kind === "contextTurn");
    expect(contextTurn?.contextKeys).toEqual(context?.contextKeys);
    expect(replies).toHaveLength(1);
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

describe("room context retention", () => {
  async function sendChatter(rt: MessagingBridgeRuntime, count: number, prefix = "room"): Promise<void> {
    for (let index = 0; index < count; index += 1) {
      await rt.handleInbound(
        inbound({ text: `chatter ${prefix}-${index}`, messageId: `${prefix}-${index}` }),
        policy,
        io(),
      );
    }
  }

  function contextRemoves(): LightspeedCall[] {
    return lightspeed.calls.filter((call) => call.kind === "contextRemove");
  }

  function appendedKeys(): string[][] {
    return lightspeed.calls
      .filter((call) => call.kind === "context")
      .map((call) => [...(call.contextKeys ?? [])]);
  }

  it("derives hierarchical room keys sharing a per-conversation prefix", async () => {
    await sendChatter(runtime, 2, "alpha");
    await runtime.handleInbound(
      inbound({
        text: "other room",
        conversationKey: "telegram:conv-2",
        chatId: "chat-2",
        messageId: "beta-0",
      }),
      policy,
      io(),
    );

    const keys = appendedKeys();
    expect(keys[0]?.[0]).toMatch(/^channel\.room\.[A-Za-z0-9_-]+\.msg\.alpha-0\.text$/);
    expect(keys[1]?.[0]).toMatch(/\.msg\.alpha-1\.text$/);
    const roomPrefix = (key: string) => key.slice(0, key.indexOf(".msg."));
    expect(roomPrefix(keys[0]?.[0] ?? "")).toBe(roomPrefix(keys[1]?.[0] ?? ""));
    expect(roomPrefix(keys[2]?.[0] ?? "")).not.toBe(roomPrefix(keys[0]?.[0] ?? ""));
  });

  it("hashes message ids outside the context-key charset", async () => {
    await runtime.handleInbound(
      inbound({ text: "odd id", messageId: "id with spaces!" }),
      policy,
      io(),
    );
    // Hashed suffix: no raw spaces, and no dots that could fake `.media.`.
    expect(appendedKeys()[0]?.[0]).toMatch(/\.msg\.[A-Za-z0-9_-]+\.text$/);
    expect(appendedKeys()[0]?.[0]).not.toContain(" ");
  });

  it("does not prune at HIGH and prunes down to LOW, oldest first, at HIGH+1", async () => {
    runtime = makeRuntime({ high: 5, low: 3 });
    await sendChatter(runtime, 5);
    expect(contextRemoves()).toHaveLength(0);

    await sendChatter(runtime, 1, "next");
    const removes = contextRemoves();
    expect(removes).toHaveLength(1);
    // 6 unconsumed messages minus LOW=3 leaves the 3 oldest to evict.
    const expected = appendedKeys().slice(0, 3).flat();
    expect(removes[0]?.removedKeys).toEqual(expected);

    // The next append stays at LOW+1 <= HIGH: no further prune.
    await sendChatter(runtime, 1, "after");
    expect(contextRemoves()).toHaveLength(1);
  });

  it("chunks a large prune into context/remove calls of at most 64 keys", async () => {
    runtime = makeRuntime({ high: 70, low: 6 });
    await sendChatter(runtime, 71);

    const removes = contextRemoves();
    expect(removes).toHaveLength(2);
    expect(removes[0]?.removedKeys).toHaveLength(64);
    expect(removes[1]?.removedKeys).toHaveLength(1);
    // 71 - LOW(6) = 65 oldest keys, in order across the chunks.
    const removed = removes.flatMap((call) => call.removedKeys ?? []);
    expect(removed).toEqual(appendedKeys().slice(0, 65).flat());
  });

  it("never prunes messages consumed by an earlier run", async () => {
    runtime = makeRuntime({ high: 4, low: 2 });
    await sendChatter(runtime, 3, "pre");
    await runtime.handleInbound(
      inbound({ text: "@lightspeed_bot summarize", mentionedBot: true, messageId: "trigger" }),
      policy,
      io(),
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();
    expect(lightspeed.calls.filter((call) => call.kind === "contextTurn")).toHaveLength(1);

    // Pre-run chatter is consumed history now. Only post-run backlog counts:
    // no prune at 4 messages, prune at 5 evicts the 3 oldest post-run ones.
    await sendChatter(runtime, 4, "post");
    expect(contextRemoves()).toHaveLength(0);
    await sendChatter(runtime, 1, "post-extra");

    const removes = contextRemoves();
    expect(removes).toHaveLength(1);
    const removed = removes[0]?.removedKeys ?? [];
    expect(removed.some((key) => key.includes(".msg.pre-"))).toBe(false);
    expect(removed.some((key) => key.includes(".msg.trigger"))).toBe(false);
    expect(removed).toEqual([
      ...(appendedKeys().find((keys) => keys[0]?.includes(".msg.post-0")) ?? []),
      ...(appendedKeys().find((keys) => keys[0]?.includes(".msg.post-1")) ?? []),
      ...(appendedKeys().find((keys) => keys[0]?.includes(".msg.post-2")) ?? []),
    ]);
  });

  it("skips pruning while a turn is queued and never removes its trigger keys", async () => {
    runtime = makeRuntime({ high: 5, low: 1 });
    // The voice note activates (default transcript carries /ask) and sits in
    // the debounce window as a queued turn with context keys.
    await runtime.handleInbound(
      inbound({
        text: "",
        messageId: "voice-1",
        fetchMedia: async () => [{ base64: "aGk=", mime: "audio/ogg" }],
      }),
      policy,
      io(),
    );
    // Chatter pushes the backlog past HIGH while the turn is queued: the
    // idle gate must skip the prune, or the run's trigger keys would vanish
    // before run/start.
    await sendChatter(runtime, 5);
    expect(contextRemoves()).toHaveLength(0);

    await new Promise((resolve) => setTimeout(resolve, 50));
    await runtime.flush();

    const contextTurn = lightspeed.calls.find((call) => call.kind === "contextTurn");
    expect(contextTurn?.contextKeys?.some((key) => key.includes(".msg.voice-1"))).toBe(true);
    expect(contextRemoves()).toHaveLength(0);
  });

  it("never calls context/remove when retention is disabled", async () => {
    runtime = makeRuntime(); // high 0 disables retention
    await sendChatter(runtime, 8);
    expect(contextRemoves()).toHaveLength(0);
  });

  it("keeps the backlog and retries after a failed removal", async () => {
    runtime = makeRuntime({ high: 3, low: 1 });
    lightspeed.failRemove = true;
    await sendChatter(runtime, 4);
    // The prune attempt failed; nothing was dropped from bookkeeping.
    expect(contextRemoves()).toHaveLength(1);

    lightspeed.failRemove = false;
    await sendChatter(runtime, 1, "retry");
    const removes = contextRemoves();
    expect(removes).toHaveLength(2);
    // The retry re-issues the previously failed keys (now one message more).
    const firstAttempt = removes[0]?.removedKeys ?? [];
    const retry = removes[1]?.removedKeys ?? [];
    expect(firstAttempt.length).toBeGreaterThan(0);
    for (const key of firstAttempt) {
      expect(retry).toContain(key);
    }
  });
});
