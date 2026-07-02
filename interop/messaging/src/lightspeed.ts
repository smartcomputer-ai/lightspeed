import {
  LightspeedClient,
  type EventCursor,
  type InputItem,
  type ProfileSource,
  type RunStatus,
  type RunView,
  type SessionConfigInput,
  type SessionItemView,
  type SessionView,
} from "@lightspeed/agent-client";
import type { LightspeedBridgeConfig } from "./config.js";
import { stableSubmissionId } from "./ids.js";
import { mediaKindForMime } from "./media.js";

export interface LightspeedTurnMedia {
  base64: string;
  mime: string;
  name?: string;
}

export interface LightspeedTurn {
  provider: string;
  accountId: string;
  conversationKey: string;
  sessionId: string;
  /// Profile to provision the session with on first creation (null = default).
  profile?: ProfileSource | null;
  /// Stable parts identifying this turn batch for submission idempotency.
  submissionParts: readonly unknown[];
  text: string;
  media?: readonly LightspeedTurnMedia[];
}

export interface LightspeedContextMessage {
  sessionId: string;
  /// Profile to provision the session with on first creation (null = default).
  profile?: ProfileSource | null;
  /// Stable base context key for this inbound message.
  key: string;
  text: string;
  media?: readonly LightspeedTurnMedia[];
}

export interface LightspeedContextAppend {
  keys: string[];
  /// Server-derived activation text per committed entry key (e.g. voice
  /// transcripts under `.media.` keys). Kept per-key so callers can weigh
  /// media-derived text differently from the echo of submitted text.
  activationText: { key: string; text: string }[];
  failures: { key: string; kind: string; message: string }[];
}

export interface LightspeedContextRemoval {
  key: string;
  /// `absent` means the key was already gone; retries are per-key no-ops.
  status: "removed" | "absent" | "failed";
  failure: { kind: string; message: string } | null;
}

export interface LightspeedReply {
  cursor: EventCursor | null;
  runId: string;
  sessionId: string;
  text: string;
  /// True when the run used a messaging tool (send/react/edit/noop). The
  /// bridge then suppresses final-text delivery: actual sends arrive via the
  /// outbox tail, and a noop means deliberate silence.
  messagingToolUsed: boolean;
}

export class LightspeedSessionBridge {
  private readonly startedSessions = new Set<string>();

  constructor(
    private readonly client: LightspeedClient,
    private readonly config: LightspeedBridgeConfig,
  ) {}

  /// Starts the session (if not already) and applies the profile through
  /// session/start. With no profile, the bridge still enables messaging tools
  /// by default so channel delivery can use the outbox.
  async ensureSession(
    sessionId: string,
    profile?: ProfileSource | null,
  ): Promise<void> {
    if (this.startedSessions.has(sessionId)) {
      return;
    }
    await this.client.call("session/start", {
      sessionId,
      cwd: this.config.cwd ?? null,
      ...(profile ? { profile } : { config: sessionStartConfig() }),
    });
    this.startedSessions.add(sessionId);
  }

  async appendMessageContext(message: LightspeedContextMessage): Promise<LightspeedContextAppend> {
    await this.ensureSession(message.sessionId, message.profile);

    const entries: { key: string; item: InputItem }[] = [];
    const text = message.text.trim();
    if (text) {
      entries.push({
        key: `${message.key}.text`,
        item: { type: "text", text },
      });
    }
    const mediaItems = await Promise.all(
      (message.media ?? []).map((media) => this.putMediaItem(media)),
    );
    for (const [index, item] of mediaItems.entries()) {
      entries.push({ key: `${message.key}.media.${index}`, item });
    }
    if (entries.length === 0) {
      return { keys: [], activationText: [], failures: [] };
    }
    const appended = await this.client.call("context/append", {
      sessionId: message.sessionId,
      entries,
    });
    const keys: string[] = [];
    const activationText: { key: string; text: string }[] = [];
    const failures: { key: string; kind: string; message: string }[] = [];
    for (const result of appended.result.results) {
      if (result.status === "failed") {
        failures.push({
          key: result.key,
          kind: result.failure?.kind ?? "unsupportedMedia",
          message: result.failure?.message ?? "context append failed",
        });
        continue;
      }
      keys.push(result.key);
      if (result.activationText?.trim()) {
        activationText.push({ key: result.key, text: result.activationText });
      }
    }
    return { keys, activationText, failures };
  }

  /// Removes active-context keys in one atomic admission signal. Callers
  /// chunk to at most 64 keys per call (the server admission cap). Removing
  /// an already-absent key is a per-key `absent` no-op, so retries after a
  /// partial prune are idempotent.
  async removeContext(
    sessionId: string,
    keys: readonly string[],
  ): Promise<LightspeedContextRemoval[]> {
    const removed = await this.client.call("context/remove", {
      sessionId,
      keys: [...keys],
    });
    return removed.result.results.map((result) => ({
      key: result.key,
      status: result.status,
      failure: result.failure
        ? {
            kind: result.failure.kind ?? "unknown",
            message: result.failure.message ?? "context remove failed",
          }
        : null,
    }));
  }

  async submitTurn(turn: LightspeedTurn): Promise<LightspeedReply> {
    await this.ensureSession(turn.sessionId, turn.profile);

    const input: InputItem[] = [
      { type: "text", text: turn.text },
      ...(await Promise.all((turn.media ?? []).map((media) => this.putMediaItem(media)))),
    ];
    const submissionId = stableSubmissionId(turn.provider, [
      turn.accountId,
      turn.conversationKey,
      ...turn.submissionParts,
    ]);
    const started = await this.client.startRun(turn.sessionId, input, { submissionId });
    return this.replyForRun(turn.sessionId, started.result.run);
  }

  async submitContextTurn(turn: Omit<LightspeedTurn, "text" | "media"> & { contextKeys: readonly string[] }): Promise<LightspeedReply> {
    await this.ensureSession(turn.sessionId, turn.profile);
    if (turn.contextKeys.length === 0) {
      throw new Error("cannot start a Lightspeed run without context keys");
    }
    const submissionId = stableSubmissionId(turn.provider, [
      turn.accountId,
      turn.conversationKey,
      ...turn.submissionParts,
    ]);
    const started = await this.client.startRunFromContext(turn.sessionId, [...turn.contextKeys], {
      submissionId,
    });
    return this.replyForRun(turn.sessionId, started.result.run);
  }

  /// Uploads one media attachment to the blob store and returns the media
  /// input item referencing it.
  private async putMediaItem(media: LightspeedTurnMedia): Promise<InputItem> {
    const put = await this.client.call("blob/put", { bytesBase64: media.base64 });
    return {
      type: "media",
      blobRef: put.result.blobRef,
      mime: media.mime,
      kind: mediaKindForMime(media.mime),
      ...(media.name !== undefined ? { name: media.name } : {}),
    };
  }

  private async replyForRun(sessionId: string, run: RunView): Promise<LightspeedReply> {
    // Do not seed this from bridge state. That cursor is process-local run
    // progress, not a durable chat cursor; starting after a terminal event can
    // make awaitRun long-poll forever even though the run is already complete.
    let cursor: EventCursor | null = null;
    const terminalStatus = terminalRunStatus(run.status);
    if (terminalStatus === "running") {
      const awaited = await this.client.awaitRun(sessionId, run.id, {
        after: cursor,
        limit: this.config.eventLimit,
        waitMs: this.config.waitMs,
        heartbeat: async (nextCursor) => {
          cursor = nextCursor;
        },
      });
      cursor = awaited.cursor;
      if (awaited.state.status === "failed") {
        throw new Error(`Lightspeed run failed: ${awaited.state.message}`);
      }
      if (awaited.state.status === "cancelled") {
        throw new Error("Lightspeed run was cancelled");
      }
    } else if (terminalStatus === "failed") {
      throw new Error("Lightspeed run failed");
    } else if (terminalStatus === "cancelled") {
      throw new Error("Lightspeed run was cancelled");
    }

    const read = await this.client.call("session/read", { sessionId });
    const text =
      extractAssistantText(read.result.session, run.id) ??
      "Lightspeed completed the run, but no assistant text was available.";
    return {
      cursor,
      runId: run.id,
      sessionId,
      text,
      messagingToolUsed: runUsedMessagingTool(read.result.session, run.id),
    };
  }
}

/// Builds the default session/start config for unprofiled conversations. The
/// bridge enables messaging tools so channel delivery can use the outbox.
export function sessionStartConfig(): SessionConfigInput {
  return {
    tools: {
      messaging: true,
    },
  };
}

/// True when the run contains at least one successful messaging tool call
/// (message_send/react/edit/noop). Failed calls do not count, so a turn whose
/// only send was rejected still falls back to final-text delivery.
export function runUsedMessagingTool(session: SessionView, runId: string): boolean {
  const run = session.runs?.find((candidate) => candidate.id === runId);
  if (!run?.items) {
    return false;
  }
  const messagingCallIds = new Set<string>();
  for (const item of run.items) {
    if (item?.type === "toolCall" && item.toolName.startsWith("message_")) {
      messagingCallIds.add(item.callId);
    }
  }
  if (messagingCallIds.size === 0) {
    return false;
  }
  return run.items.some(
    (item) =>
      item?.type === "toolResult" && !item.isError && messagingCallIds.has(item.callId),
  );
}

/// Joins every assistant message produced by the run, so multi-message runs
/// do not lose output. Falls back to the latest assistant text in the active
/// context for runs whose items are no longer addressable.
export function extractAssistantText(session: SessionView, runId?: string): string | null {
  if (runId) {
    const run = session.runs?.find((candidate) => candidate.id === runId);
    const texts = assistantTexts(run?.items);
    if (texts.length > 0) {
      return texts.join("\n\n");
    }
  }
  const fallback = assistantTexts(session.activeContext.items);
  return fallback.length > 0 ? (fallback.at(-1) ?? null) : null;
}

function assistantTexts(items: readonly SessionItemView[] | undefined): string[] {
  if (!items) {
    return [];
  }
  const texts: string[] = [];
  for (const item of items) {
    if (item?.type === "assistantMessage") {
      const text = item.text.trim();
      if (text) {
        texts.push(text);
      }
    }
  }
  return texts;
}

type TerminalRunStatus = "running" | "completed" | "failed" | "cancelled";

function terminalRunStatus(status: RunStatus): TerminalRunStatus {
  switch (status) {
    case "completed":
      return "completed";
    case "failed":
      return "failed";
    case "cancelled":
      return "cancelled";
    case "queued":
    case "running":
    case "cancelling":
      return "running";
  }
}
