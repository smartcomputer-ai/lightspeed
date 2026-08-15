import { describe, expect, it } from "vitest";
import type { ChannelSessionStartV1, NormalizedInboundV1 } from "../src/contracts/channel.js";
import type { EmissionEnvelope } from "../src/contracts/emissions.js";
import {
  applyEmission,
  applyInbound,
  compactWorkflowState,
  enqueueBoundedInbound,
  initialWorkflowState,
  MAX_CHANNEL_INBOUND_INBOX,
  snapshot,
  MAX_CHANNEL_REPLY_TARGETS,
} from "../src/workflows/state.js";

const universeId = "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f";
const invocationId = `wti:sha256:${"a".repeat(64)}`;

const start: ChannelSessionStartV1 = {
  version: 1,
  universeId,
  bindingId: "family",
  sessionId: "channel:v1:telegram:test",
  sessionKey: "family",
  scope: "direct",
  activation: {
    mode: "dm",
    triggerPrefixes: ["/ask", "/lightspeed"],
    mentionNames: [],
    batching: { debounceMs: 400, maxWaitMs: 1_500, maxMessages: 8 },
    roomContextLimit: 200,
  },
  access: { turn: "conversation", control: "admins" },
  initialRoute: {
    provider: "telegram",
    accountId: "primary",
    chatId: "123",
  },
  deliveryTaskQueue: "lightspeed-channels-delivery-v1-telegram-test",
};

const inbound: NormalizedInboundV1 = {
  version: 1,
  messageId: "42",
  route: start.initialRoute,
  senderId: "7",
  senderName: "Lukas",
  timestampMs: 1_700_000_000_000,
  text: "hello",
  isDirect: true,
  mentionedBot: false,
  isReplyToBot: false,
};

function invocation(): EmissionEnvelope {
  return {
    emission_id: invocationId,
    producer: {
      kind: "session",
      universe_id: universeId,
      session_id: start.sessionId,
      log_seq: 1,
    },
    body: {
      kind: "tool_invocation",
      invocation: {
        invocation_id: invocationId,
        tool_id: "channels.message_send.v1",
        semantic_type: "channels.message.send.v1",
        schema_revision: 1,
        binding_fingerprint: "binding:v1:test",
        session_universe_id: universeId,
        session_id: start.sessionId,
        run_id: 1,
        turn_id: 1,
        tool_batch_id: 1,
        tool_call_id: "call_1",
        arguments_ref: `sha256:${"c".repeat(64)}`,
        completion_promises: { reply: `wtp:sha256:${"d".repeat(64)}` },
      },
    },
  };
}

describe("Channel workflow state", () => {
  it("deduplicates inbound provider messages", () => {
    const state = initialWorkflowState(start);
    applyInbound(state, inbound);
    applyInbound(state, inbound);
    expect(snapshot(state)).toMatchObject({ inboundCount: 1, duplicateInboundCount: 1 });
    expect(state.snapshot.replyTargets[inbound.messageId]).toEqual({
      senderId: inbound.senderId,
      text: inbound.text,
    });
  });

  it("bounds durable native-reply context", () => {
    const state = initialWorkflowState(start);
    for (let index = 0; index < MAX_CHANNEL_REPLY_TARGETS + 4; index += 1) {
      applyInbound(state, { ...inbound, messageId: String(index), text: `message ${index}` });
    }
    expect(Object.keys(state.snapshot.replyTargets)).toHaveLength(MAX_CHANNEL_REPLY_TARGETS);
    expect(state.snapshot.replyTargets["0"]).toBeUndefined();
    expect(state.snapshot.replyTargets["4"]).toEqual({
      senderId: inbound.senderId,
      text: "message 4",
    });
  });

  it("sheds inbound signals beyond the deterministic workflow inbox ceiling", () => {
    const state = initialWorkflowState(start);
    const inbox: NormalizedInboundV1[] = [];
    for (let index = 0; index < MAX_CHANNEL_INBOUND_INBOX + 2; index += 1) {
      enqueueBoundedInbound(state, inbox, { ...inbound, messageId: String(index) });
    }
    expect(inbox).toHaveLength(MAX_CHANNEL_INBOUND_INBOX);
    expect(state.snapshot.overloadedInboundCount).toBe(2);
  });

  it("deduplicates pushed invocations before terminal handling", () => {
    const state = initialWorkflowState(start);
    const envelope = invocation();
    applyEmission(state, envelope);
    applyEmission(state, envelope);
    applyEmission(state, {
      emission_id: `emission:sha256:${"e".repeat(64)}`,
      producer: envelope.producer,
      body: {
        kind: "run_terminal",
        token: "turn:42",
        run_id: 1,
        status: "completed",
        output_ref: null,
        failure_message_ref: null,
      },
    });

    const view = snapshot(state);
    expect(Object.keys(view.invocations)).toEqual([invocationId]);
    expect(view.duplicateEmissionCount).toBe(1);
    expect(view.terminalTokens).toEqual(["turn:42"]);
  });

  it("surfaces cancellation effects independently of invocation delivery", () => {
    const state = initialWorkflowState(start);
    const effect = applyEmission(state, {
      emission_id: `emission:sha256:${"f".repeat(64)}`,
      producer: { kind: "session", universe_id: universeId, session_id: start.sessionId, log_seq: 2 },
      body: {
        kind: "invocation_cancellation",
        invocation_id: invocationId,
        completion_key: "reply",
        promise_id: `wtp:sha256:${"d".repeat(64)}`,
      },
    });
    expect(effect).toEqual({
      type: "invocation_cancelled",
      invocationId,
      promiseId: `wtp:sha256:${"d".repeat(64)}`,
    });
  });

  it("marks every message in a durable batch terminal", () => {
    const state = initialWorkflowState(start);
    state.snapshot.turns.one = {
      messageId: "1",
      submissionId: "batch",
      terminalToken: "terminal",
      status: "active",
    };
    state.snapshot.turns.two = {
      messageId: "2",
      submissionId: "batch",
      terminalToken: "terminal",
      status: "active",
    };
    applyEmission(state, {
      emission_id: `emission:sha256:${"8".repeat(64)}`,
      producer: { kind: "session", universe_id: universeId, session_id: start.sessionId, log_seq: 4 },
      body: {
        kind: "run_terminal",
        token: "terminal",
        run_id: 9,
        status: "completed",
        output_ref: null,
        failure_message_ref: null,
      },
    });
    expect(Object.values(snapshot(state).turns)).toEqual([
      expect.objectContaining({ status: "terminal", terminalStatus: "completed" }),
      expect.objectContaining({ status: "terminal", terminalStatus: "completed" }),
    ]);
  });

  it("compacts completed state while retaining active work and bounded dedup", () => {
    const state = initialWorkflowState(start);
    for (let index = 0; index < 140; index += 1) {
      state.snapshot.turns[`completed-${index}`] = {
        messageId: String(index),
        status: "terminal",
        terminalStatus: "completed",
      };
    }
    state.snapshot.turns.active = { messageId: "active", status: "active" };
    for (let index = 0; index < 2_100; index += 1) {
      state.seenInboundIds.add(`inbound-${index}`);
      state.seenEmissionIds.add(`emission-${index}`);
    }

    const carry = compactWorkflowState(state);
    expect(Object.keys(carry.snapshot.turns)).toHaveLength(129);
    expect(carry.snapshot.turns.active?.status).toBe("active");
    expect(carry.snapshot.turns["completed-0"]).toBeUndefined();
    expect(carry.seenInboundIds).toHaveLength(2_048);
    expect(carry.seenInboundIds[0]).toBe("inbound-52");

    const restored = initialWorkflowState(start, carry);
    restored.snapshot.activation.triggerPrefixes.push("/new");
    expect(carry.snapshot.activation.triggerPrefixes).not.toContain("/new");
  });
});
