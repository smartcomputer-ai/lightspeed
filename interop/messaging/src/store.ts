import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import path from "node:path";
import type { EventCursor } from "@forge/agent-client";

export interface ConversationState {
  sessionId: string;
  cursor?: EventCursor | null;
  updatedAtMs: number;
}

export interface MessageState {
  status: "processing" | "done";
  updatedAtMs: number;
  replyText?: string;
  error?: string;
}

export interface BridgeState {
  conversations: Record<string, ConversationState>;
  messages: Record<string, MessageState>;
}

const EMPTY_STATE: BridgeState = {
  conversations: {},
  messages: {},
};

export class JsonBridgeStore {
  private state: BridgeState | null = null;
  private writeChain: Promise<void> = Promise.resolve();

  constructor(private readonly filePath: string) {}

  async getOrCreateConversation(key: string, sessionId: string): Promise<ConversationState> {
    const state = await this.load();
    const existing = state.conversations[key];
    if (existing) {
      return existing;
    }
    const next: ConversationState = {
      sessionId,
      updatedAtMs: Date.now(),
    };
    state.conversations[key] = next;
    await this.persist();
    return next;
  }

  async updateCursor(key: string, cursor: EventCursor | null): Promise<void> {
    const state = await this.load();
    const existing = state.conversations[key];
    if (!existing) {
      return;
    }
    state.conversations[key] = {
      ...existing,
      cursor,
      updatedAtMs: Date.now(),
    };
    await this.persist();
  }

  async beginMessage(key: string): Promise<"new" | "duplicate" | "inflight"> {
    const state = await this.load();
    const existing = state.messages[key];
    if (existing?.status === "done") {
      return "duplicate";
    }
    if (existing?.status === "processing" && Date.now() - existing.updatedAtMs < 15 * 60_000) {
      return "inflight";
    }
    state.messages[key] = {
      status: "processing",
      updatedAtMs: Date.now(),
    };
    pruneMessages(state);
    await this.persist();
    return "new";
  }

  async markMessageDone(key: string, replyText?: string, error?: string): Promise<void> {
    const state = await this.load();
    state.messages[key] = {
      status: "done",
      updatedAtMs: Date.now(),
      ...(replyText !== undefined ? { replyText } : {}),
      ...(error !== undefined ? { error } : {}),
    };
    pruneMessages(state);
    await this.persist();
  }

  async markMessageRetryable(key: string, error: unknown): Promise<void> {
    const state = await this.load();
    state.messages[key] = {
      status: "processing",
      updatedAtMs: 0,
      error: error instanceof Error ? error.message : String(error),
    };
    await this.persist();
  }

  private async load(): Promise<BridgeState> {
    if (this.state) {
      return this.state;
    }
    try {
      this.state = JSON.parse(await readFile(this.filePath, "utf8")) as BridgeState;
    } catch {
      this.state = structuredClone(EMPTY_STATE);
    }
    this.state.conversations ??= {};
    this.state.messages ??= {};
    return this.state;
  }

  private async persist(): Promise<void> {
    const state = await this.load();
    const dir = path.dirname(this.filePath);
    const tmp = path.join(dir, `.${path.basename(this.filePath)}.${process.pid}.tmp`);
    this.writeChain = this.writeChain.then(async () => {
      await mkdir(dir, { recursive: true });
      await writeFile(tmp, `${JSON.stringify(state, null, 2)}\n`);
      await rename(tmp, this.filePath);
    });
    await this.writeChain;
  }
}

function pruneMessages(state: BridgeState): void {
  const maxAgeMs = 7 * 24 * 60 * 60_000;
  const cutoff = Date.now() - maxAgeMs;
  for (const [key, message] of Object.entries(state.messages)) {
    if (message.updatedAtMs < cutoff) {
      delete state.messages[key];
    }
  }
}
