import type { ChannelSessionStartV1, NormalizedInboundV1 } from "../contracts/channel.js";
import type {
  EmissionEnvelope,
  RunStatus,
  WorkflowToolInvocation,
} from "../contracts/emissions.js";

export interface ReceivedInvocation {
  invocation: WorkflowToolInvocation;
  producerSessionId: string;
  status: "received" | "delivering" | "resolved" | "failed" | "cancelled";
  providerMessageIds?: string[];
  resolutionEmissionIds?: string[];
  error?: string;
}

export type ApplyEmissionEffect =
  | { type: "invocation_received"; invocationId: string }
  | { type: "invocation_cancelled"; invocationId: string; promiseId: string }
  | {
      type: "run_terminal";
      token: string;
      runId: number;
      status: RunStatus;
    }
  | { type: "none" };

export interface ReceivedTurn {
  messageId: string;
  submissionId?: string;
  terminalToken?: string;
  status: "pending" | "starting" | "active" | "terminal" | "failed";
  runId?: string;
  terminalStatus?: RunStatus;
  error?: string;
}

export interface ReceivedRoomEvent {
  messageId: string;
  contextKeys: string[];
  status: "appending" | "appended" | "failed";
  contextRevision?: number;
  activationText?: string;
  error?: string;
}

export interface ChannelSessionSnapshot {
  version: 1;
  sessionId: string;
  bindingId: string;
  managedSessionCreatedAtMs: number | null;
  inboundCount: number;
  duplicateInboundCount: number;
  duplicateEmissionCount: number;
  droppedInboundCount: number;
  overloadedInboundCount: number;
  pendingTurnCount: number;
  batchCount: number;
  prunedRoomEventCount: number;
  activation: ChannelSessionStartV1["activation"];
  access: ChannelSessionStartV1["access"];
  deniedInboundCount: number;
  protocolErrors: string[];
  replyTargets: Record<string, { senderId: string; text: string }>;
  turns: Record<string, ReceivedTurn>;
  roomEvents: Record<string, ReceivedRoomEvent>;
  policyResponses: Record<
    string,
    {
      kind: "control" | "denied";
      status: "delivering" | "delivered" | "failed";
      providerMessageIds?: string[];
      error?: string;
    }
  >;
  invocations: Record<string, ReceivedInvocation>;
  terminalTokens: string[];
  cancellations: string[];
  fallbacks: Record<
    string,
    {
      status: "reconciling" | "suppressed" | "delivered" | "failed";
      providerMessageIds?: string[];
      error?: string;
    }
  >;
}

export interface ChannelWorkflowState {
  snapshot: ChannelSessionSnapshot;
  seenInboundIds: Set<string>;
  seenEmissionIds: Set<string>;
}

export interface ChannelWorkflowCarryV1 {
  version: 1;
  snapshot: ChannelSessionSnapshot;
  seenInboundIds: string[];
  seenEmissionIds: string[];
}

export const MAX_CHANNEL_REPLY_TARGETS = 256;
export const MAX_CHANNEL_INBOUND_INBOX = 256;

export function enqueueBoundedInbound<T extends NormalizedInboundV1>(
  state: ChannelWorkflowState,
  inbox: T[],
  inbound: T,
): boolean {
  if (inbox.length >= MAX_CHANNEL_INBOUND_INBOX) {
    state.snapshot.overloadedInboundCount += 1;
    return false;
  }
  inbox.push(inbound);
  return true;
}

export function initialWorkflowState(
  start: ChannelSessionStartV1,
  carry?: ChannelWorkflowCarryV1,
): ChannelWorkflowState {
  if (carry !== undefined) {
    if (
      carry.version !== 1 ||
      carry.snapshot.sessionId !== start.sessionId ||
      carry.snapshot.bindingId !== start.bindingId
    ) {
      throw new TypeError("channel workflow carry does not match the session");
    }
    return {
      snapshot: cloneSnapshot(carry.snapshot),
      seenInboundIds: new Set(carry.seenInboundIds),
      seenEmissionIds: new Set(carry.seenEmissionIds),
    };
  }
  return {
    snapshot: {
      version: 1,
      sessionId: start.sessionId,
      bindingId: start.bindingId,
      managedSessionCreatedAtMs: null,
      inboundCount: 0,
      duplicateInboundCount: 0,
      duplicateEmissionCount: 0,
      droppedInboundCount: 0,
      overloadedInboundCount: 0,
      pendingTurnCount: 0,
      batchCount: 0,
      prunedRoomEventCount: 0,
      deniedInboundCount: 0,
      activation: {
        ...start.activation,
        triggerPrefixes: [...start.activation.triggerPrefixes],
        mentionNames: [...start.activation.mentionNames],
        batching: { ...start.activation.batching },
      },
      access: { ...start.access },
      protocolErrors: [],
      replyTargets: {},
      turns: {},
      roomEvents: {},
      policyResponses: {},
      invocations: {},
      terminalTokens: [],
      cancellations: [],
      fallbacks: {},
    },
    seenInboundIds: new Set(),
    seenEmissionIds: new Set(),
  };
}

export function compactWorkflowState(state: ChannelWorkflowState): ChannelWorkflowCarryV1 {
  const compacted = snapshot(state);
  compacted.protocolErrors = compacted.protocolErrors.slice(-128);
  compacted.terminalTokens = compacted.terminalTokens.slice(-256);
  compacted.cancellations = compacted.cancellations.slice(-256);
  compacted.turns = compactRecord(
    compacted.turns,
    (turn) => turn.status !== "terminal" && turn.status !== "failed",
    128,
  );
  compacted.invocations = compactRecord(
    compacted.invocations,
    (invocation) => invocation.status === "received" || invocation.status === "delivering",
    128,
  );
  compacted.fallbacks = compactRecord(
    compacted.fallbacks,
    (fallback) => fallback.status === "reconciling",
    128,
  );
  compacted.policyResponses = compactRecord(
    compacted.policyResponses,
    (response) => response.status === "delivering",
    64,
  );
  compacted.replyTargets = Object.fromEntries(
    Object.entries(compacted.replyTargets).slice(-MAX_CHANNEL_REPLY_TARGETS),
  );
  return {
    version: 1,
    snapshot: compacted,
    seenInboundIds: [...state.seenInboundIds].slice(-2_048),
    seenEmissionIds: [...state.seenEmissionIds].slice(-2_048),
  };
}

export function applyInbound(
  state: ChannelWorkflowState,
  inbound: NormalizedInboundV1,
): string | null {
  const dedupKey = channelInboundKey(inbound);
  if (state.seenInboundIds.has(dedupKey)) {
    state.snapshot.duplicateInboundCount += 1;
    return null;
  }
  state.seenInboundIds.add(dedupKey);
  state.snapshot.inboundCount += 1;
  state.snapshot.replyTargets[inbound.messageId] = {
    senderId: inbound.senderId,
    text: inbound.text,
  };
  const replyTargetIds = Object.keys(state.snapshot.replyTargets);
  for (const expiredId of replyTargetIds.slice(0, -MAX_CHANNEL_REPLY_TARGETS)) {
    delete state.snapshot.replyTargets[expiredId];
  }
  return dedupKey;
}

export function channelInboundKey(inbound: NormalizedInboundV1): string {
  return [
    inbound.route.provider,
    inbound.route.accountId,
    inbound.route.chatId,
    inbound.route.threadId ?? "",
    inbound.messageId,
  ].join("\0");
}

export function applyEmission(
  state: ChannelWorkflowState,
  envelope: EmissionEnvelope,
): ApplyEmissionEffect {
  if (state.seenEmissionIds.has(envelope.emission_id)) {
    state.snapshot.duplicateEmissionCount += 1;
    return { type: "none" };
  }
  state.seenEmissionIds.add(envelope.emission_id);

  switch (envelope.body.kind) {
    case "tool_invocation": {
      if (envelope.producer.kind !== "session") {
        state.snapshot.protocolErrors.push("tool invocation producer is not a session");
        return { type: "none" };
      }
      const { invocation } = envelope.body;
      state.snapshot.invocations[invocation.invocation_id] = {
        invocation,
        producerSessionId: envelope.producer.session_id,
        status: "received",
      };
      return { type: "invocation_received", invocationId: invocation.invocation_id };
    }
    case "run_terminal": {
      const terminal = envelope.body;
      state.snapshot.terminalTokens.push(terminal.token);
      for (const turn of Object.values(state.snapshot.turns)) {
        if (turn.terminalToken === terminal.token) {
          turn.status = "terminal";
          turn.terminalStatus = terminal.status;
        }
      }
      return {
        type: "run_terminal",
        token: terminal.token,
        runId: terminal.run_id,
        status: terminal.status,
      };
    }
    case "invocation_cancellation":
      state.snapshot.cancellations.push(
        `${envelope.body.invocation_id}:${envelope.body.completion_key}`,
      );
      return {
        type: "invocation_cancelled",
        invocationId: envelope.body.invocation_id,
        promiseId: envelope.body.promise_id,
      };
    case "source_resolution":
      state.snapshot.protocolErrors.push("controller received an unexpected source resolution");
      return { type: "none" };
  }
}

export function snapshot(state: ChannelWorkflowState): ChannelSessionSnapshot {
  return cloneSnapshot(state.snapshot);
}

function cloneSnapshot(source: ChannelSessionSnapshot): ChannelSessionSnapshot {
  return {
    ...source,
    protocolErrors: [...source.protocolErrors],
    activation: {
      ...source.activation,
      triggerPrefixes: [...source.activation.triggerPrefixes],
      mentionNames: [...source.activation.mentionNames],
      batching: { ...source.activation.batching },
    },
    access: { ...source.access },
    replyTargets: Object.fromEntries(
      Object.entries(source.replyTargets).map(([id, target]) => [id, { ...target }]),
    ),
    turns: Object.fromEntries(
      Object.entries(source.turns).map(([id, turn]) => [id, { ...turn }]),
    ),
    invocations: Object.fromEntries(
      Object.entries(source.invocations).map(([id, entry]) => [
        id,
        {
          ...entry,
          ...(entry.providerMessageIds === undefined
            ? {}
            : { providerMessageIds: [...entry.providerMessageIds] }),
          ...(entry.resolutionEmissionIds === undefined
            ? {}
            : { resolutionEmissionIds: [...entry.resolutionEmissionIds] }),
        },
      ]),
    ),
    roomEvents: Object.fromEntries(
      Object.entries(source.roomEvents).map(([id, event]) => [
        id,
        { ...event, contextKeys: [...event.contextKeys] },
      ]),
    ),
    policyResponses: Object.fromEntries(
      Object.entries(source.policyResponses).map(([id, response]) => [
        id,
        {
          ...response,
          ...(response.providerMessageIds === undefined
            ? {}
            : { providerMessageIds: [...response.providerMessageIds] }),
        },
      ]),
    ),
    terminalTokens: [...source.terminalTokens],
    cancellations: [...source.cancellations],
    fallbacks: Object.fromEntries(
      Object.entries(source.fallbacks).map(([token, fallback]) => [
        token,
        {
          ...fallback,
          ...(fallback.providerMessageIds === undefined
            ? {}
            : { providerMessageIds: [...fallback.providerMessageIds] }),
        },
      ]),
    ),
  };
}

function compactRecord<T>(
  source: Record<string, T>,
  retain: (value: T) => boolean,
  completedLimit: number,
): Record<string, T> {
  const entries = Object.entries(source);
  const retained = new Set(entries.filter(([, value]) => retain(value)).map(([key]) => key));
  for (const [key] of entries.filter(([, value]) => !retain(value)).slice(-completedLimit)) {
    retained.add(key);
  }
  return Object.fromEntries(entries.filter(([key]) => retained.has(key)));
}
