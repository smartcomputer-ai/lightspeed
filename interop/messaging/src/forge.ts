import {
  ForgeClient,
  type EventCursor,
  type InputItem,
  type RunStatus,
  type SessionItemView,
  type SessionView,
} from "@forge/agent-client";
import type { ForgeBridgeConfig } from "./config.js";
import { stableSubmissionId } from "./ids.js";
import type { JsonBridgeStore } from "./store.js";

export interface ForgeTurnMedia {
  base64: string;
  mime: string;
  name?: string;
}

export interface ForgeTurn {
  provider: string;
  accountId: string;
  conversationKey: string;
  sessionId: string;
  /// Stable parts identifying this turn batch for submission idempotency.
  submissionParts: readonly unknown[];
  text: string;
  media?: readonly ForgeTurnMedia[];
}

export interface ForgeRoomEvent {
  key: string;
  text: string;
}

export interface ForgeReply {
  cursor: EventCursor | null;
  runId: string;
  sessionId: string;
  text: string;
}

const CONTEXT_APPEND_BATCH_LIMIT = 64;

export class ForgeSessionBridge {
  private readonly startedSessions = new Set<string>();

  constructor(
    private readonly client: ForgeClient,
    private readonly store: JsonBridgeStore,
    private readonly config: ForgeBridgeConfig,
  ) {}

  async ensureSession(sessionId: string): Promise<void> {
    if (this.startedSessions.has(sessionId)) {
      return;
    }
    await this.client.call("session/start", {
      sessionId,
      cwd: this.config.cwd ?? null,
      config: null,
    });
    this.startedSessions.add(sessionId);
  }

  /// Appends unaddressed room chatter as session context without starting a
  /// run. Idempotent per entry key, so channel redelivery is harmless.
  async appendRoomEvents(sessionId: string, events: readonly ForgeRoomEvent[]): Promise<void> {
    if (events.length === 0) {
      return;
    }
    await this.ensureSession(sessionId);
    for (let start = 0; start < events.length; start += CONTEXT_APPEND_BATCH_LIMIT) {
      const batch = events.slice(start, start + CONTEXT_APPEND_BATCH_LIMIT);
      await this.client.call("context/append", {
        sessionId,
        entries: batch.map((event) => ({
          key: event.key,
          item: { type: "text", text: event.text },
        })),
      });
    }
  }

  async submitTurn(turn: ForgeTurn): Promise<ForgeReply> {
    await this.ensureSession(turn.sessionId);

    const input: InputItem[] = [{ type: "text", text: turn.text }];
    for (const media of turn.media ?? []) {
      const put = await this.client.call("blob/put", { bytesBase64: media.base64 });
      input.push({
        type: "media",
        blobRef: put.result.blobRef,
        mime: media.mime,
        kind: "image",
        ...(media.name !== undefined ? { name: media.name } : {}),
      });
    }
    const submissionId = stableSubmissionId(turn.provider, [
      turn.accountId,
      turn.conversationKey,
      ...turn.submissionParts,
    ]);
    const started = await this.client.startRun(turn.sessionId, input, { submissionId });
    const run = started.result.run;

    const binding = await this.store.getBinding(turn.conversationKey);
    let cursor = binding?.cursor ?? null;
    const terminalStatus = terminalRunStatus(run.status);
    if (terminalStatus === "running") {
      const awaited = await this.client.awaitRun(turn.sessionId, run.id, {
        after: cursor,
        limit: this.config.eventLimit,
        waitMs: this.config.waitMs,
        heartbeat: async (nextCursor) => {
          cursor = nextCursor;
          await this.store.updateCursor(turn.conversationKey, nextCursor);
        },
      });
      cursor = awaited.cursor;
      await this.store.updateCursor(turn.conversationKey, cursor);
      if (awaited.state.status === "failed") {
        throw new Error(`Forge run failed: ${awaited.state.message}`);
      }
      if (awaited.state.status === "cancelled") {
        throw new Error("Forge run was cancelled");
      }
    } else if (terminalStatus === "failed") {
      throw new Error("Forge run failed");
    } else if (terminalStatus === "cancelled") {
      throw new Error("Forge run was cancelled");
    }

    const read = await this.client.call("session/read", { sessionId: turn.sessionId });
    const text =
      extractAssistantText(read.result.session, run.id) ??
      "Forge completed the run, but no assistant text was available.";
    return {
      cursor,
      runId: run.id,
      sessionId: turn.sessionId,
      text,
    };
  }
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
