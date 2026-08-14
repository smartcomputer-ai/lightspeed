import type { WorkflowClient } from "@temporalio/client";
import { describe, expect, it, vi } from "vitest";
import {
  CHANNEL_INBOUND_SIGNAL,
  CHANNEL_SESSION_WORKFLOW,
  CHANNELS_WORKFLOW_TASK_QUEUE,
  type ChannelSessionStartV1,
  type NormalizedInboundV1,
} from "../src/contracts/channel.js";
import { channelSessionIdentity } from "../src/identity/ids.js";
import { signalInbound } from "../src/ingress/signal.js";
import { channelSessionSearchAttributes } from "../src/contracts/search-attributes.js";

const universeId = "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f";
const identity = channelSessionIdentity({
  universeId,
  provider: "telegram",
  accountId: "primary",
  sessionKey: "family",
});
const start: ChannelSessionStartV1 = {
  version: 1,
  universeId,
  bindingId: "family",
  sessionId: identity.sessionId,
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
  initialRoute: { provider: "telegram", accountId: "primary", chatId: "123" },
  deliveryTaskQueue: identity.deliveryTaskQueue,
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
const admitted = {
  ...inbound,
  authorization: { turnAllowed: true, controlAllowed: false, memberRole: null },
} as const;

describe("signalInbound", () => {
  it("uses deterministic identity and signal-with-start", async () => {
    const signalWithStart = vi.fn(async () => ({ signaledRunId: "run-1" }));
    const client = { signalWithStart } as unknown as WorkflowClient;

    await expect(signalInbound(client, start, admitted)).resolves.toEqual({
      workflowId: identity.workflowId,
      signaledRunId: "run-1",
    });
    expect(signalWithStart).toHaveBeenCalledWith(CHANNEL_SESSION_WORKFLOW, {
      workflowId: identity.workflowId,
      taskQueue: CHANNELS_WORKFLOW_TASK_QUEUE,
      args: [start],
      signal: CHANNEL_INBOUND_SIGNAL,
      signalArgs: [admitted],
      typedSearchAttributes: channelSessionSearchAttributes(start),
    });
  });

  it("rejects identity drift before contacting Temporal", async () => {
    const signalWithStart = vi.fn();
    const client = { signalWithStart } as unknown as WorkflowClient;

    await expect(
      signalInbound(client, { ...start, sessionId: "channel:v1:telegram:wrong" }, admitted),
    ).rejects.toThrow("sessionId must be");
    expect(signalWithStart).not.toHaveBeenCalled();
  });
});
