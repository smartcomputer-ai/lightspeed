import {
  CancellationScope,
  condition,
  continueAsNew,
  defineQuery,
  defineSignal,
  getExternalWorkflowHandle,
  isCancellation,
  metricMeter,
  proxyActivities,
  setHandler,
  workflowInfo,
} from "@temporalio/workflow";
import {
  CHANNEL_INBOUND_SIGNAL,
  CHANNELS_ACTIVITY_TASK_QUEUE,
  CHANNEL_SESSION_WORKFLOW,
  CHANNEL_STATE_QUERY,
  type AdmittedChannelInboundV1,
  type ChannelSessionStartV1,
  parseAdmittedChannelInboundV1,
} from "../contracts/channel.js";
import {
  type ChannelDeliveryActivities,
  parseToolOperation,
  validateDeliveryResult,
} from "../contracts/delivery.js";
import {
  DELIVER_EMISSION_SIGNAL,
  type EmissionEnvelope,
  type PromiseResolution,
  type WorkflowToolInvocation,
  joinedReplyPromiseId,
  parseEmissionEnvelope,
  sourceResolutionEnvelope,
} from "../contracts/emissions.js";
import type { LightspeedActivities } from "../contracts/managed-session.js";
import type { ChannelInputItem, ChannelMediaActivities } from "../contracts/media.js";
import type { ChannelPresenceActivities } from "../contracts/presence.js";
import type { ControlPlaneActivities } from "../contracts/control-plane.js";
import {
  channelContextKey,
  channelTurnIdentity,
  lightspeedSessionWorkflowId,
} from "../identity/ids.js";
import { classifyInbound, formatInboundEnvelope } from "../policy/activation.js";
import {
  parseChannelControlCommand,
  type ChannelControlCommand,
} from "../policy/control.js";
import { planDeliveryCommands } from "./delivery-plan.js";
import {
  applyEmission,
  applyInbound,
  compactWorkflowState,
  enqueueBoundedInbound,
  initialWorkflowState,
  snapshot,
  type ChannelWorkflowCarryV1,
  type ChannelSessionSnapshot,
} from "./state.js";

export interface ChannelSessionWorkflowInputV1 extends ChannelSessionStartV1 {
  carry?: ChannelWorkflowCarryV1;
}

export const deliverEmissionSignal = defineSignal<[unknown]>(DELIVER_EMISSION_SIGNAL);
export const channelInboundSignal = defineSignal<[unknown]>(CHANNEL_INBOUND_SIGNAL);
export const channelStateQuery = defineQuery<ChannelSessionSnapshot>(CHANNEL_STATE_QUERY);

const {
  appendChannelContext,
  ensureManagedSession,
  putJsonBlob,
  readJsonBlob,
  reconcileTerminalRun,
  removeChannelContext,
  startChannelRun,
} = proxyActivities<LightspeedActivities>({
    taskQueue: CHANNELS_ACTIVITY_TASK_QUEUE,
    startToCloseTimeout: "30 seconds",
    retry: {
      initialInterval: "1 second",
      backoffCoefficient: 2,
      maximumInterval: "30 seconds",
    },
  });
const { assertBindingActive } = proxyActivities<ControlPlaneActivities>({
  taskQueue: CHANNELS_ACTIVITY_TASK_QUEUE,
  startToCloseTimeout: "15 seconds",
  retry: {
    initialInterval: "1 second",
    backoffCoefficient: 2,
    maximumInterval: "15 seconds",
  },
});

/**
 * Durable owner of one channel-bound managed Lightspeed session.
 *
 * Signal handlers only validate and enqueue. The dispatcher consumes inbound,
 * tool, cancellation, and terminal work independently, which is the core
 * P106 self-receiver progress requirement.
 */
export async function channelSessionWorkflowV1(
  start: ChannelSessionWorkflowInputV1,
): Promise<never> {
  validateStart(start);
  if (workflowInfo().workflowType !== CHANNEL_SESSION_WORKFLOW) {
    throw new TypeError(`unexpected workflow type: ${workflowInfo().workflowType}`);
  }

  const info = workflowInfo();
  const workflowMetrics = metricMeter.withTags({
    provider: start.initialRoute.provider,
    account_id: start.initialRoute.accountId,
    scope: start.scope,
  });
  const inboundMetric = workflowMetrics.createCounter(
    "channels_inbound",
    undefined,
    "Inbound channel messages by workflow outcome.",
  );
  const terminalMetric = workflowMetrics.createCounter(
    "channels_terminal_reconciliation",
    undefined,
    "Run-terminal reconciliation outcomes.",
  );
  const promiseMetric = workflowMetrics.createCounter(
    "channels_promise_resolution",
    undefined,
    "Runtime-owned workflow-tool reply outcomes and cancellation facts.",
  );
  const activeMetric = workflowMetrics.createGauge(
    "channels_workflow_active",
    "int",
    undefined,
    "Whether this channel workflow execution is active.",
  );
  const workflowMetricTags = { workflow_id: info.workflowId, run_id: info.runId };
  activeMetric.set(1, workflowMetricTags);

  const state = initialWorkflowState(start, start.carry);
  const inboundInbox: AdmittedChannelInboundV1[] = [];
  const emissionInbox: EmissionEnvelope[] = [];
  const pendingTurns: Array<{
    inboundKey: string;
    inbound: AdmittedChannelInboundV1;
    text: string;
    mediaItems: Array<Extract<ChannelInputItem, { type: "media" }>>;
    queuedAtMs: number;
  }> = [];
  let batchDeadlineMs: number | null = null;
  let backgroundTaskCount = 0;
  const deliveryScopes = new Map<string, CancellationScope>();
  const typingScopes = new Map<string, CancellationScope>();
  const { deliverChannelMessage } = proxyActivities<ChannelDeliveryActivities>({
    taskQueue: start.deliveryTaskQueue,
    startToCloseTimeout: "30 seconds",
    scheduleToCloseTimeout: "110 seconds",
    retry: {
      initialInterval: "1 second",
      backoffCoefficient: 2,
      maximumInterval: "15 seconds",
    },
  });
  const { prepareChannelMedia } = proxyActivities<ChannelMediaActivities>({
    taskQueue: start.deliveryTaskQueue,
    startToCloseTimeout: "90 seconds",
    scheduleToCloseTimeout: "5 minutes",
    retry: {
      initialInterval: "1 second",
      backoffCoefficient: 2,
      maximumInterval: "30 seconds",
    },
  });
  const { maintainChannelTyping } = proxyActivities<ChannelPresenceActivities>({
    taskQueue: start.deliveryTaskQueue,
    startToCloseTimeout: "24 hours",
    heartbeatTimeout: "15 seconds",
    retry: {
      initialInterval: "1 second",
      backoffCoefficient: 2,
      maximumInterval: "15 seconds",
    },
  });

  setHandler(channelInboundSignal, (raw) => {
    try {
      const inbound = parseAdmittedChannelInboundV1(raw);
      if (!enqueueBoundedInbound(state, inboundInbox, inbound)) {
        inboundMetric.add(1, { outcome: "overloaded" });
      }
    } catch (error) {
      state.snapshot.protocolErrors.push(errorMessage(error));
    }
  });
  setHandler(deliverEmissionSignal, (raw) => {
    try {
      emissionInbox.push(parseEmissionEnvelope(raw));
    } catch (error) {
      state.snapshot.protocolErrors.push(errorMessage(error));
    }
  });
  setHandler(channelStateQuery, () => snapshot(state));

  try {
  const managedSession = await ensureManagedSession({
    universeId: start.universeId,
    sessionId: start.sessionId,
    ...(start.displayName === undefined ? {} : { displayName: start.displayName }),
    ...(start.profileId === undefined ? {} : { profileId: start.profileId }),
    controller: {
      workflowId: info.workflowId,
      workflowKind: CHANNEL_SESSION_WORKFLOW,
    },
  });
  if (managedSession.sessionId !== start.sessionId) {
    throw new TypeError(
      `managed session returned ${managedSession.sessionId}, expected ${start.sessionId}`,
    );
  }
  state.snapshot.managedSessionCreatedAtMs = managedSession.createdAtMs;

  for (;;) {
    if (inboundInbox.length === 0 && emissionInbox.length === 0) {
      if (batchDeadlineMs === null) {
        await condition(() => inboundInbox.length > 0 || emissionInbox.length > 0);
      } else {
        const remainingMs = Math.max(0, batchDeadlineMs - Date.now());
        if (remainingMs > 0) {
          await condition(
            () => inboundInbox.length > 0 || emissionInbox.length > 0,
            remainingMs,
          );
        }
      }
    }
    for (const inbound of inboundInbox.splice(0)) {
      const inboundKey = applyInbound(state, inbound);
      if (inboundKey === null) {
        inboundMetric.add(1, { outcome: "deduplicated" });
        continue;
      }
      inboundMetric.add(1, { outcome: "accepted" });
      const control = parseChannelControlCommand(inbound.text);
      if (control !== null) {
        if (!inbound.authorization.controlAllowed) {
          await handleDeniedInbound(inboundKey, inbound);
        } else {
          await handleControlCommand(inboundKey, inbound, control);
        }
        continue;
      }
      if (!inbound.authorization.turnAllowed) {
        await handleDeniedInbound(inboundKey, inbound);
        continue;
      }
      const classification = classifyInbound(inbound, state.snapshot.activation);
      if (classification.kind === "drop") {
        state.snapshot.droppedInboundCount += 1;
        inboundMetric.add(1, { outcome: "dropped" });
        continue;
      }
      let mediaItems: Array<Extract<ChannelInputItem, { type: "media" }>>;
      try {
        mediaItems = await Promise.all(
          (inbound.media ?? []).map(async (media) => {
            const prepared = await prepareChannelMedia({
              universeId: start.universeId,
              route: inbound.route,
              media,
            });
            return prepared.item;
          }),
        );
      } catch (error) {
        state.snapshot.turns[inboundKey] = {
          messageId: inbound.messageId,
          status: "failed",
          error: errorMessage(error),
        };
        state.snapshot.protocolErrors.push(
          `media ${inbound.messageId}: ${errorMessage(error)}`,
        );
        continue;
      }
      if (classification.kind === "roomEvent") {
        const contextBaseKey = channelContextKey(inboundKey);
        const entries: Array<{ key: string; item: ChannelInputItem }> = [
          {
            key: `${contextBaseKey}.text`,
            item: { type: "text", text: formatInboundEnvelope(inbound, classification.text) },
          },
          ...mediaItems.map((item, index) => ({
            key: `${contextBaseKey}.media.${index}`,
            item,
          })),
        ];
        const roomEvent = {
          messageId: inbound.messageId,
          contextKeys: entries.map(({ key }) => key),
          status: "appending" as const,
        };
        state.snapshot.roomEvents[inboundKey] = roomEvent;
        try {
          const appended = await appendChannelContext({
            universeId: start.universeId,
            sessionId: start.sessionId,
            entries,
          });
          const results = new Map(appended.results.map((result) => [result.key, result]));
          for (const key of roomEvent.contextKeys) {
            if (!results.has(key)) {
              throw new Error(`context append omitted ${key}`);
            }
          }
          const activationText = roomEvent.contextKeys
            .flatMap((key) => results.get(key)?.activationText ?? [])
            .join("\n") || undefined;
          state.snapshot.roomEvents[inboundKey] = {
            ...roomEvent,
            status: "appended",
            contextRevision: appended.contextRevision,
            ...(activationText === undefined ? {} : { activationText }),
          };
          try {
            await pruneRoomContext();
          } catch (error) {
            state.snapshot.protocolErrors.push(`room context retention: ${errorMessage(error)}`);
          }
          if (activationText !== undefined) {
            const transcriptActivation = classifyInbound(
              { ...inbound, text: activationText },
              state.snapshot.activation,
            );
            if (transcriptActivation.kind === "userTurn") {
              queueTurn(inboundKey, inbound, transcriptActivation.text, mediaItems);
            }
          }
        } catch (error) {
          state.snapshot.roomEvents[inboundKey] = {
            ...roomEvent,
            status: "failed",
            error: errorMessage(error),
          };
          state.snapshot.protocolErrors.push(
            `room context ${inbound.messageId}: ${errorMessage(error)}`,
          );
        }
        continue;
      }
      queueTurn(inboundKey, inbound, classification.text, mediaItems);
    }
    while (pendingTurns.length >= state.snapshot.activation.batching.maxMessages) {
      await flushPendingTurns(state.snapshot.activation.batching.maxMessages);
    }
    for (const emission of emissionInbox.splice(0)) {
      const effect = applyEmission(state, emission);
      if (effect.type === "invocation_received") {
        const entry = state.snapshot.invocations[effect.invocationId];
        if (entry === undefined) {
          state.snapshot.protocolErrors.push(`missing invocation ${effect.invocationId}`);
          continue;
        }
        let replyPromiseId: string;
        try {
          validateInvocationOwner(start, entry.invocation);
          replyPromiseId = joinedReplyPromiseId(entry.invocation);
        } catch (error) {
          entry.status = "failed";
          entry.error = errorMessage(error);
          state.snapshot.protocolErrors.push(
            `invalid invocation ${effect.invocationId}: ${errorMessage(error)}`,
          );
          continue;
        }
        const cancellation = state.snapshot.cancellations.find((fact) =>
          fact.startsWith(`${effect.invocationId}:`),
        );
        if (cancellation !== undefined) {
          entry.status = "cancelled";
          backgroundTaskCount += 1;
          void CancellationScope.nonCancellable(async () => {
            entry.resolutionEmissionIds = await resolvePromises(
              start,
              info.workflowId,
              [replyPromiseId],
              { kind: "cancelled" },
            );
            promiseMetric.add(1, { outcome: "cancelled" });
          }).catch((error) => {
            entry.error = errorMessage(error);
            state.snapshot.protocolErrors.push(
              `cancel invocation ${effect.invocationId}: ${errorMessage(error)}`,
            );
          }).finally(() => {
            backgroundTaskCount -= 1;
          });
          continue;
        }
        entry.status = "delivering";
        const scope = new CancellationScope({ cancellable: true });
        deliveryScopes.set(effect.invocationId, scope);
        backgroundTaskCount += 1;
        void scope
          .run(() =>
            processInvocation(
              start,
              info.workflowId,
              entry.invocation,
              replyPromiseId,
              deliverChannelMessage,
            ),
          )
          .then(({ messageIds, resolutionEmissionIds }) => {
            entry.status = "resolved";
            entry.providerMessageIds = messageIds;
            entry.resolutionEmissionIds = resolutionEmissionIds;
          })
          .catch(async (error) => {
            if (isCancellation(error)) {
              entry.status = "cancelled";
              entry.resolutionEmissionIds = await CancellationScope.nonCancellable(() =>
                resolvePromises(start, info.workflowId, [replyPromiseId], { kind: "cancelled" }),
              );
              promiseMetric.add(1, { outcome: "cancelled" });
              return;
            }
            entry.status = "failed";
            entry.error = errorMessage(error);
            state.snapshot.protocolErrors.push(
              `invocation ${effect.invocationId}: ${errorMessage(error)}`,
            );
          })
          .finally(() => {
            deliveryScopes.delete(effect.invocationId);
            backgroundTaskCount -= 1;
          })
          .catch((error) => {
            entry.error = errorMessage(error);
            state.snapshot.protocolErrors.push(
              `invocation cleanup ${effect.invocationId}: ${errorMessage(error)}`,
            );
          });
      }
      if (effect.type === "invocation_cancelled") {
        promiseMetric.add(1, { outcome: "cancellation_received" });
        deliveryScopes.get(effect.invocationId)?.cancel();
      }
      if (effect.type === "run_terminal") {
        terminalMetric.add(1, { outcome: "received", run_status: effect.status });
        typingScopes.get(effect.token)?.cancel();
        const fallback = { status: "reconciling" as const };
        state.snapshot.fallbacks[effect.token] = fallback;
        backgroundTaskCount += 1;
        void reconcileTerminalRun({
          universeId: start.universeId,
          sessionId: start.sessionId,
          runId: effect.runId,
          status: effect.status,
        })
          .then(async (reconciliation) => {
            if (reconciliation.action === "suppress") {
              state.snapshot.fallbacks[effect.token] = { status: "suppressed" };
              terminalMetric.add(1, { outcome: "suppressed", run_status: effect.status });
              return;
            }
            await assertBindingActive({
              bindingId: start.bindingId,
              route: start.initialRoute,
              scope: start.scope,
            });
            const delivered = await deliverPlanned(
              `fallback:${start.sessionId}:${effect.runId}`,
              { type: "send", text: reconciliation.text },
              deliverChannelMessage,
            );
            state.snapshot.fallbacks[effect.token] = {
              status: "delivered",
              providerMessageIds: delivered.messageIds,
            };
            terminalMetric.add(1, { outcome: "fallback_delivered", run_status: effect.status });
          })
          .catch((error) => {
            terminalMetric.add(1, { outcome: "failed", run_status: effect.status });
            state.snapshot.fallbacks[effect.token] = {
              status: "failed",
              error: errorMessage(error),
            };
            state.snapshot.protocolErrors.push(
              `terminal ${effect.token}: ${errorMessage(error)}`,
            );
          })
          .finally(() => {
            backgroundTaskCount -= 1;
          });
      }
    }
    if (batchDeadlineMs !== null && batchDeadlineMs <= Date.now()) {
      await flushPendingTurns(pendingTurns.length);
    }
    if (
      workflowInfo().continueAsNewSuggested &&
      inboundInbox.length === 0 &&
      emissionInbox.length === 0 &&
      pendingTurns.length === 0 &&
      deliveryScopes.size === 0 &&
      typingScopes.size === 0 &&
      backgroundTaskCount === 0
    ) {
      await continueAsNew<typeof channelSessionWorkflowV1>({
        ...start,
        carry: compactWorkflowState(state),
      });
    }
  }
  } finally {
    activeMetric.set(0, workflowMetricTags);
  }

  function scheduleBatchDeadline(): void {
    const first = pendingTurns[0];
    const last = pendingTurns.at(-1);
    if (first === undefined || last === undefined) {
      batchDeadlineMs = null;
      return;
    }
    const batching = state.snapshot.activation.batching;
    batchDeadlineMs = Math.min(
      first.queuedAtMs + batching.maxWaitMs,
      last.queuedAtMs + batching.debounceMs,
    );
  }

  function queueTurn(
    inboundKey: string,
    inbound: AdmittedChannelInboundV1,
    text: string,
    mediaItems: Array<Extract<ChannelInputItem, { type: "media" }>>,
  ): void {
    pendingTurns.push({ inboundKey, inbound, text, mediaItems, queuedAtMs: Date.now() });
    state.snapshot.turns[inboundKey] = { messageId: inbound.messageId, status: "pending" };
    state.snapshot.pendingTurnCount = pendingTurns.length;
    scheduleBatchDeadline();
  }

  async function handleDeniedInbound(
    inboundKey: string,
    inbound: AdmittedChannelInboundV1,
  ): Promise<void> {
    state.snapshot.deniedInboundCount += 1;
    inboundMetric.add(1, { outcome: "denied" });
    if (!inbound.isDirect) {
      return;
    }
    await sendPolicyResponse(
      inboundKey,
      inbound,
      "denied",
      "This channel identity is not authorized for this Lightspeed universe.",
    );
  }

  async function handleControlCommand(
    inboundKey: string,
    inbound: AdmittedChannelInboundV1,
    command: ChannelControlCommand,
  ): Promise<void> {
    if (pendingTurns.length > 0) {
      await flushPendingTurns(pendingTurns.length);
    }
    let text: string;
    if (command.kind === "activation") {
      if (start.scope === "direct") {
        text = "Direct chats are always active; /activation applies to groups.";
      } else {
        state.snapshot.activation.mode = command.mode;
        text = `Group activation is now ${command.mode}.`;
      }
    } else if (command.kind === "activation_help") {
      text = "Usage: /activation mention|always|silent";
    } else {
      text = [
        `activation: ${state.snapshot.activation.mode}`,
        `session: ${start.sessionId}`,
        "commands: /activation mention|always|silent, /status",
      ].join("\n");
    }
    await sendPolicyResponse(inboundKey, inbound, "control", text);
  }

  async function sendPolicyResponse(
    inboundKey: string,
    inbound: AdmittedChannelInboundV1,
    kind: "control" | "denied",
    text: string,
  ): Promise<void> {
    const response = { kind, status: "delivering" as const };
    state.snapshot.policyResponses[inboundKey] = response;
    try {
      await assertBindingActive({
        bindingId: start.bindingId,
        route: start.initialRoute,
        scope: start.scope,
      });
      const delivered = await deliverPlanned(
        `policy:${start.sessionId}:${inbound.messageId}`,
        {
          type: "send",
          text,
          replyTo: inbound.messageId,
          replyContext: { senderId: inbound.senderId, text: inbound.text },
        },
        deliverChannelMessage,
      );
      state.snapshot.policyResponses[inboundKey] = {
        kind,
        status: "delivered",
        providerMessageIds: delivered.messageIds,
      };
    } catch (error) {
      state.snapshot.policyResponses[inboundKey] = {
        kind,
        status: "failed",
        error: errorMessage(error),
      };
      state.snapshot.protocolErrors.push(`policy response ${inbound.messageId}: ${errorMessage(error)}`);
    }
  }

  async function pruneRoomContext(): Promise<void> {
    const appended = Object.entries(state.snapshot.roomEvents).filter(
      ([, event]) => event.status === "appended",
    );
    const excess = appended.length - state.snapshot.activation.roomContextLimit;
    if (excess <= 0) {
      return;
    }
    const targets = appended.slice(0, excess);
    const removed = await removeChannelContext({
      universeId: start.universeId,
      sessionId: start.sessionId,
      keys: targets.flatMap(([, event]) => event.contextKeys),
    });
    const accepted = new Set(
      removed.results
        .filter((result) => result.status === "removed" || result.status === "absent")
        .map((result) => result.key),
    );
    for (const [inboundKey, event] of targets) {
      if (event.contextKeys.every((key) => accepted.has(key))) {
        delete state.snapshot.roomEvents[inboundKey];
        state.snapshot.prunedRoomEventCount += 1;
      }
    }
  }

  async function flushPendingTurns(count: number): Promise<void> {
    if (count === 0) {
      return;
    }
    const batch = pendingTurns.splice(0, count);
    scheduleBatchDeadline();
    state.snapshot.pendingTurnCount = pendingTurns.length;
    const batchKey = batch.map((turn) => turn.inboundKey).join("\u001f");
    const turnIdentity = channelTurnIdentity(info.workflowId, `batch\0${batchKey}`);
    for (const pending of batch) {
      state.snapshot.turns[pending.inboundKey] = {
        messageId: pending.inbound.messageId,
        submissionId: turnIdentity.submissionId,
        terminalToken: turnIdentity.terminalToken,
        status: "starting",
      };
    }
    state.snapshot.batchCount += 1;
    const items: ChannelInputItem[] = batch.flatMap((pending) => [
      {
        type: "text" as const,
        text: formatInboundEnvelope(pending.inbound, pending.text),
      },
      ...pending.mediaItems,
    ]);
    try {
      const started = await startChannelRun({
        universeId: start.universeId,
        sessionId: start.sessionId,
        submissionId: turnIdentity.submissionId,
        terminalToken: turnIdentity.terminalToken,
        items,
      });
      startTyping(turnIdentity.terminalToken);
      for (const pending of batch) {
        const turn = state.snapshot.turns[pending.inboundKey];
        if (turn !== undefined) {
          state.snapshot.turns[pending.inboundKey] = {
            ...turn,
            status: "active",
            runId: started.runId,
          };
        }
      }
    } catch (error) {
      for (const pending of batch) {
        const turn = state.snapshot.turns[pending.inboundKey];
        if (turn !== undefined) {
          state.snapshot.turns[pending.inboundKey] = {
            ...turn,
            status: "failed",
            error: errorMessage(error),
          };
        }
      }
    }
  }

  function startTyping(terminalToken: string): void {
    if (typingScopes.has(terminalToken)) return;
    const scope = new CancellationScope({ cancellable: true });
    typingScopes.set(terminalToken, scope);
    backgroundTaskCount += 1;
    void scope
      .run(() => maintainChannelTyping({ route: start.initialRoute }))
      .catch((error) => {
        if (!isCancellation(error)) {
          state.snapshot.protocolErrors.push(`typing ${terminalToken}: ${errorMessage(error)}`);
        }
      })
      .finally(() => {
        typingScopes.delete(terminalToken);
        backgroundTaskCount -= 1;
      });
  }

  async function processInvocation(
    workflowStart: ChannelSessionStartV1,
    producerWorkflowId: string,
    invocation: WorkflowToolInvocation,
    replyPromiseId: string,
    deliver: ChannelDeliveryActivities["deliverChannelMessage"],
  ): Promise<{ messageIds: string[]; resolutionEmissionIds: string[] }> {
    try {
      await assertBindingActive({
        bindingId: workflowStart.bindingId,
        route: workflowStart.initialRoute,
        scope: workflowStart.scope,
      });
      const rawArguments = await readJsonBlob({
        universeId: workflowStart.universeId,
        blobRef: invocation.arguments_ref,
      });
      const operation = attachReplyContext(
        parseToolOperation(invocation.tool_id, rawArguments),
      );
      const delivered = await deliverPlanned(
        invocation.invocation_id,
        operation,
        deliver,
      );
      const receipt = await putJsonBlob({
        universeId: workflowStart.universeId,
        value: { provider: delivered.provider, messageIds: delivered.messageIds },
      });
      const resolutionEmissionIds = await resolvePromises(
        workflowStart,
        producerWorkflowId,
        [replyPromiseId],
        { kind: "resolved", payload_ref: receipt.blobRef },
      );
      promiseMetric.add(1, { outcome: "resolved" });
      return { messageIds: delivered.messageIds, resolutionEmissionIds };
    } catch (error) {
      if (isCancellation(error)) {
        throw error;
      }
      const errorRef = await putErrorBlob(workflowStart.universeId, error);
      await resolvePromises(workflowStart, producerWorkflowId, [replyPromiseId], {
        kind: "failed",
        error_ref: errorRef,
      });
      promiseMetric.add(1, { outcome: "failed" });
      throw error;
    }
  }

  async function deliverPlanned(
    invocationId: string,
    operation: ReturnType<typeof parseToolOperation>,
    deliver: ChannelDeliveryActivities["deliverChannelMessage"],
  ): Promise<{ provider: ChannelSessionStartV1["initialRoute"]["provider"]; messageIds: string[] }> {
    const messageIds: string[] = [];
    for (const command of planDeliveryCommands(invocationId, start.initialRoute, operation)) {
      const result = validateDeliveryResult(
        await deliver(command),
        start.initialRoute.provider,
      );
      messageIds.push(...result.messageIds);
    }
    return { provider: start.initialRoute.provider, messageIds };
  }

  function attachReplyContext(
    operation: ReturnType<typeof parseToolOperation>,
  ): ReturnType<typeof parseToolOperation> {
    if (operation.type !== "send" || operation.replyTo == null) {
      return operation;
    }
    const replyContext = state.snapshot.replyTargets[operation.replyTo];
    return replyContext === undefined ? operation : { ...operation, replyContext };
  }

  async function putErrorBlob(universeId: string, error: unknown): Promise<string | null> {
    try {
      return (
        await putJsonBlob({
          universeId,
          value: { error: errorMessage(error) },
        })
      ).blobRef;
    } catch {
      return null;
    }
  }

  async function resolvePromises(
    workflowStart: ChannelSessionStartV1,
    producerWorkflowId: string,
    promiseIds: string[],
    resolution: PromiseResolution,
  ): Promise<string[]> {
    const holder = getExternalWorkflowHandle(
      lightspeedSessionWorkflowId(workflowStart.universeId, workflowStart.sessionId),
    );
    const emissionIds: string[] = [];
    for (const promiseId of promiseIds) {
      const envelope = sourceResolutionEnvelope({
        universeId: workflowStart.universeId,
        producerWorkflowId,
        promiseId,
        resolution,
      });
      await holder.signal(DELIVER_EMISSION_SIGNAL, envelope);
      emissionIds.push(envelope.emission_id);
    }
    return emissionIds;
  }
}

function validateInvocationOwner(
  start: ChannelSessionStartV1,
  invocation: WorkflowToolInvocation,
): void {
  if (
    invocation.session_universe_id !== start.universeId ||
    invocation.session_id !== start.sessionId
  ) {
    throw new TypeError("pushed invocation does not belong to this managed session");
  }
}

function validateStart(start: ChannelSessionStartV1): void {
  if (start.version !== 1) {
    throw new TypeError("channel session start version must be 1");
  }
  if (start.scope !== "direct" && start.scope !== "group") {
    throw new TypeError("channel session scope must be direct or group");
  }
  if (
    (start.scope === "direct" && start.activation.mode !== "dm") ||
    (start.scope === "group" && start.activation.mode === "dm")
  ) {
    throw new TypeError("channel activation mode must match the session scope");
  }
  if (
    (start.access.turn !== "conversation" && start.access.turn !== "members") ||
    (start.access.control !== "none" &&
      start.access.control !== "members" &&
      start.access.control !== "admins" &&
      start.access.control !== "owners")
  ) {
    throw new TypeError("channel access policy is invalid");
  }
  for (const [key, value] of Object.entries({
    universeId: start.universeId,
    bindingId: start.bindingId,
    sessionId: start.sessionId,
    sessionKey: start.sessionKey,
    scope: start.scope,
    deliveryTaskQueue: start.deliveryTaskQueue,
  })) {
    if (typeof value !== "string" || value.length === 0) {
      throw new TypeError(`${key} must be a non-empty string`);
    }
  }
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
