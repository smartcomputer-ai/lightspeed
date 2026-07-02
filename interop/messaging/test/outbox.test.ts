import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import type { OutboundMessageView, SessionView } from "@lightspeed/agent-client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { runUsedMessagingTool } from "../src/lightspeed.js";
import {
  DeliveryError,
  OutboxTailer,
  type ChannelDeliverer,
  type OutboxTailerOptions,
} from "../src/outbox.js";
import { JsonBridgeStore, type BindingState } from "../src/store.js";

type AckRecord = { outboxId: string; result: { type: string; retryable?: boolean } };

function entry(seq: number, sessionId: string, text: string): OutboundMessageView {
  return {
    seq,
    outboxId: `outbox_${seq}`,
    sessionId,
    runId: "run_1",
    origin: "toolCall",
    payload: { type: "send", text },
    attempts: 0,
    createdAtMs: 1,
  };
}

class FakeRpc {
  pending: OutboundMessageView[] = [];
  acks: AckRecord[] = [];
  appends: Array<{ sessionId: string; key: string; text: string }> = [];

  asClient(): OutboxTailerOptions["client"] {
    return this as unknown as OutboxTailerOptions["client"];
  }

  call = async (method: string, params: Record<string, unknown>) => {
    if (method === "outbox/read") {
      const after = (params.after as number) ?? 0;
      const entries = this.pending.filter((candidate) => candidate.seq > after);
      return {
        result: {
          entries,
          nextAfter: entries.at(-1)?.seq ?? after,
        },
      };
    }
    if (method === "context/append") {
      const entries = params.entries as Array<{ key: string; item: { text: string } }>;
      for (const entry of entries) {
        this.appends.push({
          sessionId: params.sessionId as string,
          key: entry.key,
          text: entry.item.text,
        });
      }
      return {
        result: {
          contextRevision: 1,
          results: entries.map((entry) => ({ key: entry.key, status: "applied" })),
        },
      };
    }
    if (method === "outbox/ack") {
      this.acks.push({
        outboxId: params.outboxId as string,
        result: params.result as AckRecord["result"],
      });
      this.pending = this.pending.filter(
        (candidate) => candidate.outboxId !== params.outboxId,
      );
      return { result: { outboxId: params.outboxId, status: "delivered", attempts: 1 } };
    }
    throw new Error(`unexpected method ${method}`);
  };
}

let dir: string;
let store: JsonBridgeStore;

beforeEach(async () => {
  dir = await mkdtemp(path.join(tmpdir(), "bridge-outbox-"));
  store = new JsonBridgeStore(path.join(dir, "state.json"));
  await store.getOrCreateBinding("conv-1", {
    channel: "telegram",
    accountId: "default",
    chatId: "chat-1",
    sessionId: "session_bound",
    activation: "dm",
  });
});

afterEach(async () => {
  await rm(dir, { recursive: true, force: true });
});

function deliverer(
  deliver: ChannelDeliverer["deliver"],
  overrides?: Partial<Pick<ChannelDeliverer, "channel" | "accountId">>,
): ChannelDeliverer {
  return {
    channel: overrides?.channel ?? "telegram",
    accountId: overrides?.accountId ?? "default",
    deliver,
  };
}

describe("OutboxTailer", () => {
  it("delivers pending entries through the bound channel and acks", async () => {
    const rpc = new FakeRpc();
    rpc.pending = [entry(1, "session_bound", "hello"), entry(2, "session_bound", "world")];
    const delivered: string[] = [];
    const tailer = new OutboxTailer({
      client: rpc.asClient(),
      store,
      log: () => undefined,
      deliverers: [
        deliverer(async (_binding: BindingState, payload) => {
          if (payload.type === "send") {
            delivered.push(payload.text);
          }
          return { channelMessageId: "tg-1" };
        }),
      ],
    });

    expect(await tailer.tick()).toBe(2);
    expect(delivered).toEqual(["hello", "world"]);
    expect(rpc.acks.map((ack) => ack.result.type)).toEqual(["delivered", "delivered"]);
    // Delivered sends are recorded back into the session so the model
    // learns its own channel message ids.
    expect(rpc.appends).toHaveLength(2);
    expect(rpc.appends[0]?.sessionId).toBe("session_bound");
    expect(rpc.appends[0]?.text).toContain("#tg-1");
    expect(rpc.appends[0]?.text).toContain("hello");
  });

  it("parks entries for unknown sessions and retries disconnected channels", async () => {
    const rpc = new FakeRpc();
    rpc.pending = [entry(1, "session_unknown", "lost"), entry(2, "session_bound", "later")];
    const tailer = new OutboxTailer({
      client: rpc.asClient(),
      store,
      log: () => undefined,
      deliverers: [deliverer(async () => ({}), { channel: "whatsapp" })],
    });

    await tailer.tick();
    expect(rpc.acks).toEqual([
      {
        outboxId: "outbox_1",
        result: expect.objectContaining({ type: "failed", retryable: false }),
      },
      {
        outboxId: "outbox_2",
        result: expect.objectContaining({ type: "failed", retryable: true }),
      },
    ]);
  });

  it("propagates the retryable flag from delivery errors", async () => {
    const rpc = new FakeRpc();
    rpc.pending = [entry(1, "session_bound", "bad")];
    const tailer = new OutboxTailer({
      client: rpc.asClient(),
      store,
      log: () => undefined,
      deliverers: [
        deliverer(async () => {
          throw new DeliveryError("message to react not found", false);
        }),
      ],
    });

    await tailer.tick();
    expect(rpc.acks[0]?.result).toEqual(
      expect.objectContaining({ type: "failed", retryable: false }),
    );
  });
});

describe("runUsedMessagingTool", () => {
  function sessionWith(
    items: NonNullable<NonNullable<SessionView["runs"]>[number]["items"]>,
  ): SessionView {
    return {
      activeContext: { revision: 1, items: [] },
      configRevision: 0,
      createdAtMs: 1,
      id: "session_1",
      runs: [{ id: "run_1", source: { type: "input", items: [] }, items, status: "completed" }],
      status: "idle",
      updatedAtMs: 2,
    };
  }

  it("detects a successful messaging tool call", () => {
    const session = sessionWith([
      { id: "t1", type: "toolCall", callId: "c1", toolName: "message_send", status: "succeeded" },
      { id: "r1", type: "toolResult", callId: "c1", isError: false, status: "succeeded" },
      { id: "a1", type: "assistantMessage", text: "internal notes" },
    ]);
    expect(runUsedMessagingTool(session, "run_1")).toBe(true);
  });

  it("ignores failed messaging tool calls and other tools", () => {
    const failed = sessionWith([
      { id: "t1", type: "toolCall", callId: "c1", toolName: "message_send", status: "succeeded" },
      { id: "r1", type: "toolResult", callId: "c1", isError: true, status: "failed" },
    ]);
    expect(runUsedMessagingTool(failed, "run_1")).toBe(false);

    const otherTool = sessionWith([
      { id: "t1", type: "toolCall", callId: "c1", toolName: "web_fetch", status: "succeeded" },
      { id: "r1", type: "toolResult", callId: "c1", isError: false, status: "succeeded" },
    ]);
    expect(runUsedMessagingTool(otherTool, "run_1")).toBe(false);
  });

  it("counts message_noop as a messaging interaction", () => {
    const session = sessionWith([
      { id: "t1", type: "toolCall", callId: "c1", toolName: "message_noop", status: "succeeded" },
      { id: "r1", type: "toolResult", callId: "c1", isError: false, status: "succeeded" },
    ]);
    expect(runUsedMessagingTool(session, "run_1")).toBe(true);
  });
});
