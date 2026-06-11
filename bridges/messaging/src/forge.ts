import {
  ForgeClient,
  type EventCursor,
  type InputItem,
  type RunStatus,
  type SessionItemView,
  type SessionView,
} from "@forge/agent-client";
import type { ForgeBridgeConfig } from "./config.js";
import { stableSessionId, stableSubmissionId } from "./ids.js";
import type { JsonBridgeStore } from "./store.js";

export interface ForgeInboundText {
  provider: string;
  accountId: string;
  conversationKey: string;
  conversationParts: readonly unknown[];
  messageId: string;
  text: string;
}

export interface ForgeReply {
  cursor: EventCursor | null;
  runId: string;
  sessionId: string;
  text: string;
}

export class ForgeSessionBridge {
  constructor(
    private readonly client: ForgeClient,
    private readonly store: JsonBridgeStore,
    private readonly config: ForgeBridgeConfig,
  ) {}

  async submitText(message: ForgeInboundText): Promise<ForgeReply> {
    const sessionId = stableSessionId(this.config.sessionPrefix, message.conversationParts);
    const conversation = await this.store.getOrCreateConversation(message.conversationKey, sessionId);

    await this.client.call("session/start", {
      sessionId: conversation.sessionId,
      cwd: this.config.cwd ?? null,
      config: null,
    });

    const input: InputItem[] = [{ type: "text", text: message.text }];
    const submissionId = stableSubmissionId(message.provider, [
      message.accountId,
      message.conversationKey,
      message.messageId,
    ]);
    const started = await this.client.startRun(conversation.sessionId, input, { submissionId });
    const run = started.result.run;

    let cursor = conversation.cursor ?? null;
    const terminalStatus = terminalRunStatus(run.status);
    if (terminalStatus === "running") {
      const awaited = await this.client.awaitRun(conversation.sessionId, run.id, {
        after: cursor,
        limit: this.config.eventLimit,
        waitMs: this.config.waitMs,
        heartbeat: async (nextCursor) => {
          cursor = nextCursor;
          await this.store.updateCursor(message.conversationKey, nextCursor);
        },
      });
      cursor = awaited.cursor;
      await this.store.updateCursor(message.conversationKey, cursor);
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

    const read = await this.client.call("session/read", { sessionId: conversation.sessionId });
    const text =
      extractLatestAssistantText(read.result.session, run.id) ??
      "Forge completed the run, but no assistant text was available.";
    return {
      cursor,
      runId: run.id,
      sessionId: conversation.sessionId,
      text,
    };
  }
}

export function extractLatestAssistantText(session: SessionView, runId?: string): string | null {
  if (runId) {
    const run = session.runs?.find((candidate) => candidate.id === runId);
    const runText = latestAssistantText(run?.items);
    if (runText) {
      return runText;
    }
  }
  return latestAssistantText(session.activeContext.items);
}

function latestAssistantText(items: readonly SessionItemView[] | undefined): string | null {
  if (!items) {
    return null;
  }
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item?.type === "assistantMessage") {
      const text = item.text.trim();
      if (text) {
        return text;
      }
    }
  }
  return null;
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
