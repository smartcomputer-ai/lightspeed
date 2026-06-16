import {
  LightspeedClient,
  type EventCursor,
  type InputItem,
  type RunStatus,
  type SessionConfigInput,
  type SessionItemView,
  type SessionView,
  type VfsMountSourceInput,
} from "@lightspeed/agent-client";
import type { LightspeedBridgeConfig, SessionRecipe } from "./config.js";
import { stableSubmissionId } from "./ids.js";
import { mediaKindForMime } from "./media.js";
import type { JsonBridgeStore } from "./store.js";

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
  /// Recipe to provision the session with on first creation (null = default).
  recipe?: SessionRecipe | null;
  /// Stable parts identifying this turn batch for submission idempotency.
  submissionParts: readonly unknown[];
  text: string;
  media?: readonly LightspeedTurnMedia[];
}

export interface LightspeedRoomEvent {
  key: string;
  text: string;
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

const CONTEXT_APPEND_BATCH_LIMIT = 64;

export class LightspeedSessionBridge {
  private readonly startedSessions = new Set<string>();

  constructor(
    private readonly client: LightspeedClient,
    private readonly store: JsonBridgeStore,
    private readonly config: LightspeedBridgeConfig,
  ) {}

  /// Starts the session (if not already) and applies the recipe once: the
  /// session config (model + tools), then workspace/snapshot mounts, then MCP
  /// links. The core discovers prompts and the skill catalog from the mounts,
  /// so the bridge never activates skills or sets instructions itself.
  /// session/start is idempotent and the config only applies on creation;
  /// mounts and links are guarded by `startedSessions` so they run once.
  async ensureSession(sessionId: string, recipe?: SessionRecipe | null): Promise<void> {
    if (this.startedSessions.has(sessionId)) {
      return;
    }
    await this.client.call("session/start", {
      sessionId,
      cwd: this.config.cwd ?? null,
      config: sessionStartConfig(recipe),
    });
    for (const mount of recipe?.mounts ?? []) {
      await this.client.call("vfs/mount/put", {
        sessionId,
        mountPath: mount.mountPath,
        source: mountSourceInput(mount.source),
        access: mount.access,
      });
    }
    for (const link of recipe?.mcp ?? []) {
      await this.client.call("session/mcp/link", {
        sessionId,
        serverId: link.serverId,
        ...(link.serverLabel !== undefined ? { serverLabel: link.serverLabel } : {}),
        ...(link.toolId !== undefined ? { toolId: link.toolId } : {}),
        ...(link.allowedTools !== undefined ? { allowedTools: link.allowedTools } : {}),
        ...(link.approval !== undefined ? { approval: link.approval } : {}),
        ...(link.authGrantId !== undefined ? { authGrantId: link.authGrantId } : {}),
        ...(link.deferLoading !== undefined ? { deferLoading: link.deferLoading } : {}),
      });
    }
    this.startedSessions.add(sessionId);
  }

  /// Appends unaddressed room chatter as session context without starting a
  /// run. Idempotent per entry key, so channel redelivery is harmless.
  async appendRoomEvents(
    sessionId: string,
    events: readonly LightspeedRoomEvent[],
    recipe?: SessionRecipe | null,
  ): Promise<void> {
    if (events.length === 0) {
      return;
    }
    await this.ensureSession(sessionId, recipe);
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

  async submitTurn(turn: LightspeedTurn): Promise<LightspeedReply> {
    await this.ensureSession(turn.sessionId, turn.recipe);

    const input: InputItem[] = [{ type: "text", text: turn.text }];
    for (const media of turn.media ?? []) {
      const put = await this.client.call("blob/put", { bytesBase64: media.base64 });
      input.push({
        type: "media",
        blobRef: put.result.blobRef,
        mime: media.mime,
        kind: mediaKindForMime(media.mime),
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

    const read = await this.client.call("session/read", { sessionId: turn.sessionId });
    const text =
      extractAssistantText(read.result.session, run.id) ??
      "Lightspeed completed the run, but no assistant text was available.";
    return {
      cursor,
      runId: run.id,
      sessionId: turn.sessionId,
      text,
      messagingToolUsed: runUsedMessagingTool(read.result.session, run.id),
    };
  }
}

/// Builds the session/start config from a recipe, defaulting the messaging
/// toolset on (the bridge delivers via the outbox) unless the recipe sets it
/// explicitly. All other config fields (model, generation, host tool mode,
/// web tools, etc.) pass through from the recipe untouched.
export function sessionStartConfig(recipe?: SessionRecipe | null): SessionConfigInput {
  const base = recipe?.config ?? {};
  const tools = base.tools ?? {};
  return {
    ...base,
    tools: {
      ...tools,
      messaging: tools.messaging ?? true,
    },
  };
}

function mountSourceInput(source: SessionRecipe["mounts"][number]["source"]): VfsMountSourceInput {
  if ("workspaceId" in source) {
    return { type: "workspace", workspaceId: source.workspaceId };
  }
  return { type: "snapshot", snapshotRef: source.snapshotRef };
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
