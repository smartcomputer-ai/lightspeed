import { fileURLToPath } from "node:url";
import { TestWorkflowEnvironment } from "@temporalio/testing";
import { Worker } from "@temporalio/worker";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { createFakeDeliveryActivities } from "../src/activities/fake-delivery.js";
import {
  CHANNEL_INBOUND_SIGNAL,
  CHANNEL_SESSION_WORKFLOW,
  CHANNEL_STATE_QUERY,
  CHANNELS_ACTIVITY_TASK_QUEUE,
  CHANNELS_WORKFLOW_TASK_QUEUE,
  type ChannelSessionStartV1,
} from "../src/contracts/channel.js";
import type { EmissionEnvelope } from "../src/contracts/emissions.js";
import type { ChannelDeliveryCommandV1 } from "../src/contracts/delivery.js";
import type { ChannelInputItem, PrepareChannelMediaInput } from "../src/contracts/media.js";
import { channelSessionIdentity, lightspeedSessionWorkflowId } from "../src/identity/ids.js";
import type { ChannelSessionSnapshot } from "../src/workflows/state.js";

const runIntegration = process.env.LIGHTSPEED_CHANNELS_TEMPORAL_INTEGRATION === "1";

describe.runIf(runIntegration)("Channels self-receiver", () => {
  let env: TestWorkflowEnvironment;

  beforeAll(async () => {
    env = await TestWorkflowEnvironment.createLocal();
  }, 120_000);

  afterAll(async () => {
    await env.teardown();
  });

  it("delivers a pushed Joined tool and resolves its runtime-owned reply", async () => {
    const workflowWorker = await Worker.create({
      connection: env.nativeConnection,
      namespace: env.namespace ?? "default",
      taskQueue: CHANNELS_WORKFLOW_TASK_QUEUE,
      workflowsPath: fileURLToPath(new URL("./workflows.ts", import.meta.url)),
    });
    const receiptRef = `sha256:${"b".repeat(64)}`;
    const fakeDelivery = createFakeDeliveryActivities();
    let deliveryCalls = 0;
    const startedInputs = new Map<string, ChannelInputItem[]>();
    const preparedMedia: PrepareChannelMediaInput[] = [];
    const activityWorker = await Worker.create({
      connection: env.nativeConnection,
      namespace: env.namespace ?? "default",
      taskQueue: CHANNELS_ACTIVITY_TASK_QUEUE,
      activities: {
        ensureManagedSession: async ({ sessionId }: { sessionId: string }) => ({
          sessionId,
          createdAtMs: 1234,
        }),
        readJsonBlob: async () => ({ text: "x".repeat(3_600) }),
        putJsonBlob: async () => ({ blobRef: receiptRef }),
        startChannelRun: async ({
          submissionId,
          items,
        }: {
          submissionId: string;
          items: ChannelInputItem[];
        }) => {
          startedInputs.set(submissionId, items);
          return { runId: "1" };
        },
        prepareChannelMedia: async (input: PrepareChannelMediaInput) => {
          preparedMedia.push(input);
          return {
            item: {
              type: "media" as const,
              blobRef: `sha256:${"9".repeat(64)}`,
              kind: input.media.kind,
              mime: input.media.mime,
              ...(input.media.name === undefined ? {} : { name: input.media.name }),
            },
          };
        },
        maintainChannelTyping: async () => undefined,
        appendChannelContext: async () => ({ contextRevision: 1, results: [] }),
        removeChannelContext: async () => ({ contextRevision: 1, results: [] }),
        reconcileTerminalRun: async () => ({
          action: "suppress" as const,
          reason: "messaging_tool" as const,
        }),
        assertBindingActive: async () => undefined,
        deliverChannelMessage: async (command: ChannelDeliveryCommandV1) => {
          deliveryCalls += 1;
          return fakeDelivery.deliverChannelMessage(command);
        },
      },
    });
    const workflowRun = workflowWorker.run();
    const activityRun = activityWorker.run();

    try {
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
          batching: { debounceMs: 20, maxWaitMs: 100, maxMessages: 8 },
          roomContextLimit: 200,
        },
        access: { turn: "conversation", control: "admins" },
        initialRoute: { provider: "telegram", accountId: "primary", chatId: "123" },
        deliveryTaskQueue: CHANNELS_ACTIVITY_TASK_QUEUE,
      };
      const holder = await env.client.workflow.start("testHolderWorkflow", {
        workflowId: lightspeedSessionWorkflowId(universeId, start.sessionId),
        taskQueue: CHANNELS_WORKFLOW_TASK_QUEUE,
      });
      const channel = await env.client.workflow.signalWithStart(CHANNEL_SESSION_WORKFLOW, {
        workflowId: identity.workflowId,
        taskQueue: CHANNELS_WORKFLOW_TASK_QUEUE,
        args: [start],
        signal: CHANNEL_INBOUND_SIGNAL,
        signalArgs: [
          {
            version: 1,
            messageId: "42",
            route: start.initialRoute,
            senderId: "7",
            senderName: "Lukas",
            timestampMs: 1_700_000_000_000,
            text: "hello",
            media: [
              {
                version: 1,
                provider: "telegram",
                fileId: "telegram-file-1",
                kind: "image",
                mime: "image/jpeg",
                name: "photo.jpg",
                byteSize: 123,
              },
            ],
            isDirect: true,
            mentionedBot: false,
            isReplyToBot: false,
            authorization: { turnAllowed: true, controlAllowed: false, memberRole: null },
          },
        ],
      });
      const invocationId = `wti:sha256:${"a".repeat(64)}`;
      const promiseId = `wtp:sha256:${"d".repeat(64)}`;
      const pushed: EmissionEnvelope = {
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
            completion_promises: { reply: promiseId },
          },
        },
      };
      await channel.signal("deliver_emission", pushed);

      const state = await eventually(() => channel.query<ChannelSessionSnapshot>(CHANNEL_STATE_QUERY),
        (snapshot) => snapshot.invocations[invocationId]?.status === "resolved");
      const resolutions = await eventually(
        () => holder.query<EmissionEnvelope[]>("holder_state"),
        (emissions) => emissions.length === 1,
      );

      expect(state.invocations[invocationId]).toMatchObject({
        status: "resolved",
        providerMessageIds: [
          `fake:${invocationId}:chunk:1/2`,
          `fake:${invocationId}:chunk:2/2`,
        ],
      });
      expect(preparedMedia).toHaveLength(1);
      expect(preparedMedia[0]).not.toHaveProperty("bytes");
      await eventually(
        async () => startedInputs.size,
        (size) => size === 1,
      );
      expect([...startedInputs.values()][0]).toEqual([
        {
          type: "text",
          text: "[telegram:dm #42] Lukas (2023-11-14 22:13Z): hello",
        },
        {
          type: "media",
          blobRef: `sha256:${"9".repeat(64)}`,
          kind: "image",
          mime: "image/jpeg",
          name: "photo.jpg",
        },
      ]);
      expect(resolutions[0]).toMatchObject({
        producer: { kind: "workflow", workflow_id: identity.workflowId },
        body: {
          kind: "source_resolution",
          promise_id: promiseId,
          resolution: { kind: "resolved", payload_ref: receiptRef },
        },
      });

      await channel.signal("deliver_emission", pushed);
      const stateBeforeTerminal = await channel.query<ChannelSessionSnapshot>(CHANNEL_STATE_QUERY);
      const terminalToken = Object.values(stateBeforeTerminal.turns)[0]?.terminalToken;
      expect(terminalToken).toBeDefined();
      await channel.signal("deliver_emission", {
        emission_id: `emission:sha256:${"e".repeat(64)}`,
        producer: pushed.producer,
        body: {
          kind: "run_terminal",
          token: terminalToken ?? "missing-terminal-token",
          run_id: 1,
          status: "completed",
          output_ref: null,
          failure_message_ref: null,
        },
      } satisfies EmissionEnvelope);
      const deduplicated = await eventually(
        () => channel.query<ChannelSessionSnapshot>(CHANNEL_STATE_QUERY),
        (snapshot) =>
          snapshot.duplicateEmissionCount === 1 &&
          snapshot.terminalTokens.includes(terminalToken ?? "missing-terminal-token") &&
          snapshot.fallbacks[terminalToken ?? "missing-terminal-token"]?.status === "suppressed",
      );
      expect(deduplicated.invocations[invocationId]?.status).toBe("resolved");
      expect(Object.values(deduplicated.turns)[0]).toMatchObject({
        status: "terminal",
        runId: "1",
        terminalStatus: "completed",
      });
      expect(await holder.query<EmissionEnvelope[]>("holder_state")).toHaveLength(1);

      const cancelledInvocationId = `wti:sha256:${"1".repeat(64)}`;
      const cancelledPromiseId = `wtp:sha256:${"2".repeat(64)}`;
      await channel.signal("deliver_emission", {
        emission_id: `emission:sha256:${"3".repeat(64)}`,
        producer: pushed.producer,
        body: {
          kind: "invocation_cancellation",
          invocation_id: cancelledInvocationId,
          completion_key: "reply",
          promise_id: cancelledPromiseId,
        },
      } satisfies EmissionEnvelope);
      await channel.signal("deliver_emission", {
        emission_id: cancelledInvocationId,
        producer: {
          kind: "session",
          universe_id: universeId,
          session_id: start.sessionId,
          log_seq: 3,
        },
        body: {
          kind: "tool_invocation",
          invocation: {
            ...(pushed.body.kind === "tool_invocation"
              ? pushed.body.invocation
              : neverInvocation()),
            invocation_id: cancelledInvocationId,
            tool_call_id: "call_cancelled",
            completion_promises: { reply: cancelledPromiseId },
          },
        },
      } satisfies EmissionEnvelope);
      const cancelled = await eventually(
        () => channel.query<ChannelSessionSnapshot>(CHANNEL_STATE_QUERY),
        (snapshot) => snapshot.invocations[cancelledInvocationId]?.status === "cancelled",
      );
      expect(cancelled.invocations[cancelledInvocationId]?.status).toBe("cancelled");
      const afterCancellation = await eventually(
        () => holder.query<EmissionEnvelope[]>("holder_state"),
        (emissions) => emissions.length === 2,
      );
      expect(afterCancellation[1]?.body).toMatchObject({
        kind: "source_resolution",
        promise_id: cancelledPromiseId,
        resolution: { kind: "cancelled" },
      });
      expect(deliveryCalls).toBe(2);

      const history = await channel.fetchHistory();
      await Worker.runReplayHistory(
        { workflowsPath: fileURLToPath(new URL("./workflows.ts", import.meta.url)) },
        history,
        identity.workflowId,
      );
    } finally {
      workflowWorker.shutdown();
      activityWorker.shutdown();
      await Promise.all([workflowRun, activityRun]);
    }
  }, 60_000);

  it("persists ambient group context and batches addressed turns on workflow timers", async () => {
    const workflowWorker = await Worker.create({
      connection: env.nativeConnection,
      namespace: env.namespace ?? "default",
      taskQueue: CHANNELS_WORKFLOW_TASK_QUEUE,
      workflowsPath: fileURLToPath(new URL("./workflows.ts", import.meta.url)),
    });
    const startedInputs = new Map<string, ChannelInputItem[]>();
    let contextAppends = 0;
    const activityWorker = await Worker.create({
      connection: env.nativeConnection,
      namespace: env.namespace ?? "default",
      taskQueue: CHANNELS_ACTIVITY_TASK_QUEUE,
      activities: {
        ensureManagedSession: async ({ sessionId }: { sessionId: string }) => ({
          sessionId,
          createdAtMs: 5678,
        }),
        appendChannelContext: async ({ entries }: { entries: Array<{ key: string }> }) => {
          contextAppends += 1;
          return {
            contextRevision: contextAppends,
            results: entries.map(({ key }) => ({ key, status: "applied" as const })),
          };
        },
        removeChannelContext: async ({ keys }: { keys: string[] }) => ({
          contextRevision: contextAppends + 1,
          results: keys.map((key) => ({ key, status: "removed" as const })),
        }),
        startChannelRun: async ({
          submissionId,
          items,
        }: {
          submissionId: string;
          items: ChannelInputItem[];
        }) => {
          startedInputs.set(submissionId, items);
          return { runId: String(startedInputs.size) };
        },
        maintainChannelTyping: async () => undefined,
        readJsonBlob: async () => ({}),
        putJsonBlob: async () => ({ blobRef: `sha256:${"b".repeat(64)}` }),
        reconcileTerminalRun: async () => ({ action: "suppress" as const, reason: "messaging_tool" as const }),
        assertBindingActive: async () => undefined,
        deliverChannelMessage: createFakeDeliveryActivities().deliverChannelMessage,
      },
    });
    const workflowRun = workflowWorker.run();
    const activityRun = activityWorker.run();

    try {
      const universeId = "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f";
      const identity = channelSessionIdentity({
        universeId,
        provider: "telegram",
        accountId: "primary",
        sessionKey: "team",
      });
      const start: ChannelSessionStartV1 = {
        version: 1,
        universeId,
        bindingId: "team",
        sessionId: identity.sessionId,
        sessionKey: "team",
        scope: "group",
        activation: {
          mode: "mention",
          triggerPrefixes: ["/ask"],
          mentionNames: ["lightspeed"],
          batching: { debounceMs: 100, maxWaitMs: 500, maxMessages: 8 },
          roomContextLimit: 1,
        },
        access: { turn: "conversation", control: "admins" },
        initialRoute: { provider: "telegram", accountId: "primary", chatId: "-100" },
        deliveryTaskQueue: CHANNELS_ACTIVITY_TASK_QUEUE,
      };
      const inbound = (
        messageId: string,
        text: string,
        mentionedBot: boolean,
        controlAllowed = false,
      ) => ({
        version: 1 as const,
        messageId,
        route: start.initialRoute,
        senderId: messageId,
        senderName: `Sender ${messageId}`,
        timestampMs: 1_700_000_000_000 + Number(messageId),
        text,
        isDirect: false,
        mentionedBot,
        isReplyToBot: false,
        authorization: { turnAllowed: true, controlAllowed, memberRole: null },
      });
      const channel = await env.client.workflow.signalWithStart(CHANNEL_SESSION_WORKFLOW, {
        workflowId: identity.workflowId,
        taskQueue: CHANNELS_WORKFLOW_TASK_QUEUE,
        args: [start],
        signal: CHANNEL_INBOUND_SIGNAL,
        signalArgs: [inbound("1", "ambient context", false)],
      });
      await channel.signal(CHANNEL_INBOUND_SIGNAL, inbound("4", "newer ambient context", false));
      await channel.signal(CHANNEL_INBOUND_SIGNAL, inbound("2", "@lightspeed first", true));
      await channel.signal(CHANNEL_INBOUND_SIGNAL, inbound("3", "/ask second", false));
      await channel.signal(CHANNEL_INBOUND_SIGNAL, inbound("5", "/activation always", false, true));
      await channel.signal(CHANNEL_INBOUND_SIGNAL, inbound("6", "active after toggle", false));
      await channel.signal(CHANNEL_INBOUND_SIGNAL, inbound("7", "/status", false));

      const state = await eventually(
        () => channel.query<ChannelSessionSnapshot>(CHANNEL_STATE_QUERY),
        (snapshot) =>
          snapshot.roomEvents[Object.keys(snapshot.roomEvents)[0] ?? ""]?.status === "appended" &&
          snapshot.prunedRoomEventCount === 1 &&
          snapshot.batchCount === 2 &&
          snapshot.activation.mode === "always" &&
          snapshot.deniedInboundCount === 1 &&
          Object.values(snapshot.policyResponses).some(
            (response) => response.kind === "control" && response.status === "delivered",
          ) &&
          Object.values(snapshot.turns).every((turn) => turn.status === "active"),
      );
      expect(contextAppends).toBe(2);
      expect(Object.keys(state.roomEvents)).toHaveLength(1);
      expect(startedInputs.size).toBe(2);
      const uniqueStartedInputs = [...startedInputs.values()];
      const firstBatchText = uniqueStartedInputs[0]
        ?.filter((item): item is Extract<ChannelInputItem, { type: "text" }> => item.type === "text")
        .map((item) => item.text)
        .join("\n");
      const secondBatchText = uniqueStartedInputs[1]
        ?.filter((item): item is Extract<ChannelInputItem, { type: "text" }> => item.type === "text")
        .map((item) => item.text)
        .join("\n");
      expect(firstBatchText).toContain("Sender 2");
      expect(firstBatchText).toContain("first");
      expect(firstBatchText).toContain("Sender 3");
      expect(firstBatchText).toContain("second");
      expect(secondBatchText).toContain("active after toggle");
      const turnByMessageId = new Map(
        Object.values(state.turns).map((turn) => [turn.messageId, turn]),
      );
      expect(turnByMessageId.get("2")?.submissionId).toBe(
        turnByMessageId.get("3")?.submissionId,
      );
      expect(turnByMessageId.get("6")?.submissionId).not.toBe(
        turnByMessageId.get("2")?.submissionId,
      );
      expect(state.pendingTurnCount).toBe(0);

      const history = await channel.fetchHistory();
      await Worker.runReplayHistory(
        { workflowsPath: fileURLToPath(new URL("./workflows.ts", import.meta.url)) },
        history,
        identity.workflowId,
      );
    } finally {
      workflowWorker.shutdown();
      activityWorker.shutdown();
      await Promise.all([workflowRun, activityRun]);
    }
  }, 60_000);
});

function neverInvocation(): never {
  throw new Error("expected a tool invocation fixture");
}

async function eventually<T>(
  read: () => Promise<T>,
  ready: (value: T) => boolean,
): Promise<T> {
  const deadline = Date.now() + 20_000;
  for (;;) {
    const value = await read();
    if (ready(value)) {
      return value;
    }
    if (Date.now() >= deadline) {
      throw new Error("timed out waiting for workflow state");
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}
